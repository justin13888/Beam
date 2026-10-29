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

/// The shared behavioural contract every [`KeyValueStore`] must meet.
///
/// Instantiated over [`InMemoryKeyValueStore`] below, and meant for every
/// other implementation of the trait: the same assertions constraining the
/// double and the platform's store are what make the double legitimate, where
/// tests of the double alone proved only that a `BTreeMap` works.
///
/// `$make` is an expression producing a fresh, empty store; it is evaluated
/// once per case, and must implement [`KeyValueStore`] by value.
#[macro_export]
macro_rules! key_value_store_contract {
    ($make:expr) => {
        use $crate::ports::kv::KeyValueStore as _;

        fn key(name: &str) -> String {
            name.to_owned()
        }

        #[tokio::test]
        async fn a_written_value_reads_back_and_a_second_write_replaces_it() {
            let store = $make;
            store
                .put(key("servers/index"), key("[]"))
                .await
                .expect("put");
            assert_eq!(
                store.get(key("servers/index")).await.expect("get"),
                Some(key("[]"))
            );

            store
                .put(key("servers/index"), key("[\"a\"]"))
                .await
                .expect("put");
            assert_eq!(
                store.get(key("servers/index")).await.expect("get"),
                Some(key("[\"a\"]"))
            );
        }

        #[tokio::test]
        async fn an_absent_key_reads_as_none_and_removing_it_succeeds() {
            let store = $make;
            assert_eq!(store.get(key("nothing")).await.expect("get"), None);
            store.remove(key("nothing")).await.expect("remove");
            assert_eq!(
                store.get_secret(key("nothing")).await.expect("get_secret"),
                None
            );
            store
                .remove_secret(key("nothing"))
                .await
                .expect("remove_secret");
        }

        #[tokio::test]
        async fn a_removed_value_reads_as_none() {
            let store = $make;
            store.put(key("k"), key("v")).await.expect("put");
            store.remove(key("k")).await.expect("remove");
            assert_eq!(store.get(key("k")).await.expect("get"), None);
        }

        #[tokio::test]
        async fn a_secret_reads_back_is_replaced_and_is_removed() {
            let store = $make;
            store
                .put_secret(key("session/a"), key("first"))
                .await
                .expect("put_secret");
            store
                .put_secret(key("session/a"), key("second"))
                .await
                .expect("put_secret");
            assert_eq!(
                store
                    .get_secret(key("session/a"))
                    .await
                    .expect("get_secret"),
                Some(key("second"))
            );

            store
                .remove_secret(key("session/a"))
                .await
                .expect("remove_secret");
            assert_eq!(
                store
                    .get_secret(key("session/a"))
                    .await
                    .expect("get_secret"),
                None
            );
        }

        /// The whole point of the split boundary: a secret is not reachable
        /// through the plaintext methods, and a plaintext value is not
        /// reachable through the secret ones, under the same key.
        #[tokio::test]
        async fn secrets_and_plaintext_are_separate_namespaces() {
            let store = $make;
            store
                .put_secret(key("session/a"), key("cookie"))
                .await
                .expect("put_secret");
            assert_eq!(store.get(key("session/a")).await.expect("get"), None);

            store
                .put(key("servers/a"), key("record"))
                .await
                .expect("put");
            assert_eq!(
                store
                    .get_secret(key("servers/a"))
                    .await
                    .expect("get_secret"),
                None
            );

            // Removing one never removes the other.
            store.put(key("both"), key("plain")).await.expect("put");
            store
                .put_secret(key("both"), key("secret"))
                .await
                .expect("put_secret");
            store.remove(key("both")).await.expect("remove");
            assert_eq!(
                store.get_secret(key("both")).await.expect("get_secret"),
                Some(key("secret"))
            );
        }

        #[tokio::test]
        async fn list_keys_is_every_live_plaintext_key_with_the_prefix() {
            let store = $make;
            store.put(key("servers/a"), key("1")).await.expect("put");
            store.put(key("servers/b"), key("2")).await.expect("put");
            store.put(key("servers/gone"), key("3")).await.expect("put");
            store.remove(key("servers/gone")).await.expect("remove");
            store.put(key("pins/a"), key("4")).await.expect("put");
            store
                .put_secret(key("servers/secret"), key("s"))
                .await
                .expect("put_secret");

            let mut keys = store.list_keys(key("servers/")).await.expect("list_keys");
            keys.sort();
            assert_eq!(keys, vec![key("servers/a"), key("servers/b")]);
            assert!(
                store
                    .list_keys(key("absent/"))
                    .await
                    .expect("list_keys")
                    .is_empty()
            );
        }
    };
}

#[cfg(test)]
mod contract_over_in_memory {
    crate::key_value_store_contract!(super::InMemoryKeyValueStore::new());
}
