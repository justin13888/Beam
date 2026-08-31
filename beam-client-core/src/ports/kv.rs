//! The persistence boundary, implemented by the foreign side.
//!
//! The core deliberately owns no storage of its own. Android has a real
//! keystore and a real preferences store, iOS has a real keychain; a Rust
//! reimplementation would be worse than either and could not reach the
//! hardware-backed key material both platforms already have.

use crate::error::StorageError;

/// Key/value persistence supplied by the platform.
///
/// Secrets are separate methods rather than a flag on the ordinary ones. That
/// is not ceremony: it means the foreign implementation cannot accidentally
/// route a session cookie into plaintext preferences, because the plaintext
/// path has no way to express one.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait KeyValueStore: Send + Sync + std::fmt::Debug {
    /// Read a plaintext value.
    async fn get(&self, key: String) -> Result<Option<String>, StorageError>;

    /// Write a plaintext value, replacing any existing one.
    async fn put(&self, key: String, value: String) -> Result<(), StorageError>;

    /// Remove a plaintext value. Removing an absent key succeeds.
    async fn remove(&self, key: String) -> Result<(), StorageError>;

    /// Every plaintext key carrying `prefix`, in unspecified order.
    async fn list_keys(&self, prefix: String) -> Result<Vec<String>, StorageError>;

    /// Read a secret. Backed by the platform keystore.
    async fn get_secret(&self, key: String) -> Result<Option<String>, StorageError>;

    /// Write a secret, replacing any existing one.
    async fn put_secret(&self, key: String, value: String) -> Result<(), StorageError>;

    /// Remove a secret. Removing an absent key succeeds.
    async fn remove_secret(&self, key: String) -> Result<(), StorageError>;
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::{FailureMode, InMemoryKeyValueStore};

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
mod in_memory {
    use super::{KeyValueStore, StorageError};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Which operations an [`InMemoryKeyValueStore`] should fail.
    ///
    /// Exists so the "storage refused" branches are reachable from tests. The
    /// repository's testing mandate is that any edge case which would
    /// otherwise need manual verification is codified instead, and a locked
    /// keystore is exactly such a case.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum FailureMode {
        /// Behave like working storage.
        #[default]
        None,
        /// Fail reads; writes still succeed.
        FailReads,
        /// Fail writes; reads still succeed.
        FailWrites,
        /// Fail everything.
        FailAll,
    }

    /// A stateful in-memory [`KeyValueStore`], for tests and for the Rust side
    /// of platform-independent behaviour.
    ///
    /// Real state transitions, not canned expectations: a value written is a
    /// value subsequently read. Secrets are held in a separate map so a test
    /// can assert that a cookie never reached plaintext storage.
    #[derive(Debug, Default)]
    pub struct InMemoryKeyValueStore {
        plain: Mutex<BTreeMap<String, String>>,
        secret: Mutex<BTreeMap<String, String>>,
        failure: Mutex<FailureMode>,
    }

    impl InMemoryKeyValueStore {
        /// An empty store that behaves normally.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Make subsequent operations fail as described.
        pub fn set_failure(&self, mode: FailureMode) {
            *self.failure.lock().expect("failure lock") = mode;
        }

        /// Whether `key` exists in *plaintext* storage.
        ///
        /// Distinct from [`Self::has_secret`] so a test can prove a secret was
        /// not written to the wrong place.
        #[must_use]
        pub fn has_plain(&self, key: &str) -> bool {
            self.plain.lock().expect("plain lock").contains_key(key)
        }

        /// Whether `key` exists in secret storage.
        #[must_use]
        pub fn has_secret(&self, key: &str) -> bool {
            self.secret.lock().expect("secret lock").contains_key(key)
        }

        /// How many entries are held across both maps.
        #[must_use]
        pub fn len(&self) -> usize {
            self.plain.lock().expect("plain lock").len()
                + self.secret.lock().expect("secret lock").len()
        }

        /// Whether the store holds nothing at all.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }

        fn check(&self, writing: bool) -> Result<(), StorageError> {
            let mode = *self.failure.lock().expect("failure lock");
            let fails = match mode {
                FailureMode::None => false,
                FailureMode::FailAll => true,
                FailureMode::FailReads => !writing,
                FailureMode::FailWrites => writing,
            };
            if fails {
                return Err(StorageError::Unavailable {
                    detail: "in-memory store configured to fail".to_owned(),
                });
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl KeyValueStore for InMemoryKeyValueStore {
        async fn get(&self, key: String) -> Result<Option<String>, StorageError> {
            self.check(false)?;
            Ok(self.plain.lock().expect("plain lock").get(&key).cloned())
        }

        async fn put(&self, key: String, value: String) -> Result<(), StorageError> {
            self.check(true)?;
            self.plain.lock().expect("plain lock").insert(key, value);
            Ok(())
        }

        async fn remove(&self, key: String) -> Result<(), StorageError> {
            self.check(true)?;
            self.plain.lock().expect("plain lock").remove(&key);
            Ok(())
        }

        async fn list_keys(&self, prefix: String) -> Result<Vec<String>, StorageError> {
            self.check(false)?;
            Ok(self
                .plain
                .lock()
                .expect("plain lock")
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect())
        }

        async fn get_secret(&self, key: String) -> Result<Option<String>, StorageError> {
            self.check(false)?;
            Ok(self.secret.lock().expect("secret lock").get(&key).cloned())
        }

        async fn put_secret(&self, key: String, value: String) -> Result<(), StorageError> {
            self.check(true)?;
            self.secret.lock().expect("secret lock").insert(key, value);
            Ok(())
        }

        async fn remove_secret(&self, key: String) -> Result<(), StorageError> {
            self.check(true)?;
            self.secret.lock().expect("secret lock").remove(&key);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_written_value_reads_back() {
        let store = InMemoryKeyValueStore::new();
        store
            .put("servers/index".to_owned(), "[]".to_owned())
            .await
            .expect("put");
        assert_eq!(
            store.get("servers/index".to_owned()).await.expect("get"),
            Some("[]".to_owned())
        );
    }

    #[tokio::test]
    async fn an_absent_key_reads_as_none_and_removes_cleanly() {
        let store = InMemoryKeyValueStore::new();
        assert_eq!(store.get("nothing".to_owned()).await.expect("get"), None);
        store.remove("nothing".to_owned()).await.expect("remove");
    }

    #[tokio::test]
    async fn secrets_are_held_apart_from_plaintext() {
        let store = InMemoryKeyValueStore::new();
        store
            .put_secret("session/a".to_owned(), "cookie".to_owned())
            .await
            .expect("put_secret");

        // The whole point of the split boundary: a secret must not be
        // reachable through, or land in, the plaintext store.
        assert!(store.has_secret("session/a"));
        assert!(!store.has_plain("session/a"));
        assert_eq!(store.get("session/a".to_owned()).await.expect("get"), None);
    }

    #[tokio::test]
    async fn list_keys_filters_by_prefix_and_ignores_secrets() {
        let store = InMemoryKeyValueStore::new();
        store.put("servers/a".to_owned(), "1".to_owned()).await.ok();
        store.put("servers/b".to_owned(), "2".to_owned()).await.ok();
        store.put("pins/a".to_owned(), "3".to_owned()).await.ok();
        store
            .put_secret("servers/secret".to_owned(), "s".to_owned())
            .await
            .ok();

        let mut keys = store
            .list_keys("servers/".to_owned())
            .await
            .expect("list_keys");
        keys.sort();
        assert_eq!(keys, vec!["servers/a".to_owned(), "servers/b".to_owned()]);
    }

    #[tokio::test]
    async fn failure_injection_reaches_reads_and_writes_independently() {
        let store = InMemoryKeyValueStore::new();
        store
            .put("k".to_owned(), "v".to_owned())
            .await
            .expect("put");

        store.set_failure(FailureMode::FailReads);
        assert!(store.get("k".to_owned()).await.is_err());
        assert!(store.put("k".to_owned(), "v2".to_owned()).await.is_ok());

        store.set_failure(FailureMode::FailWrites);
        assert!(store.get("k".to_owned()).await.is_ok());
        assert!(store.put("k".to_owned(), "v3".to_owned()).await.is_err());

        store.set_failure(FailureMode::None);
        assert!(store.get("k".to_owned()).await.is_ok());
    }
}
