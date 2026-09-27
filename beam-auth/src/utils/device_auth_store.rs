//! Storage for in-flight RFC 8628 device authorization grants (ADR-0017).
//!
//! A native client that starts a device login is handed an opaque *handle*;
//! the IdP's device code stays here, keyed by the handle's SHA-256, so only
//! Beam can redeem the grant. Every client poll goes through
//! [`DeviceAuthStore::claim_poll`], which is the rate limit RFC 8628 section
//! 3.5 asks of a client, enforced by the server on the client's behalf: a
//! flow may reach the IdP at most once per `interval_secs`, and a poll that
//! comes early is refused before the IdP is contacted.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    sea_query::Expr,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use beam_domain::services::{Clock, RealClock};
use beam_entity::device_auth::{
    ActiveModel as DeviceAuthActiveModel, Column, Entity as DeviceAuthEntity,
};

/// How much a `slow_down` grows a flow's interval by (RFC 8628 section 3.5).
pub const SLOW_DOWN_STEP_SECS: u32 = 5;

/// A device login as it is started: what the IdP answered, and the hash of
/// the handle the client will poll with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDeviceAuth {
    pub handle_hash: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval_secs: u32,
    pub expires_in_secs: u64,
}

/// A stored device login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuth {
    pub handle_hash: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval_secs: u32,
    pub next_poll_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// What asking to poll concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// No flow has this handle: it never existed, or has already ended.
    NotFound,
    /// The flow outlived its lifetime. It is left in place for the caller to
    /// consume.
    Expired,
    /// The previous poll was less than `interval_secs` ago. The IdP must not
    /// be contacted.
    TooEarly { interval_secs: u32 },
    /// This poll may reach the IdP; the next one may not come before
    /// `interval_secs` from now.
    Claimed(DeviceAuth),
}

#[derive(Debug, Error)]
pub enum DeviceAuthError {
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

type Result<T> = std::result::Result<T, DeviceAuthError>;

#[async_trait]
pub trait DeviceAuthStore: Send + Sync + std::fmt::Debug {
    /// Persists a new flow, pollable immediately, expiring `expires_in_secs`
    /// from now. Sweeps every already-expired flow as it goes, so abandoned
    /// flows do not accumulate.
    async fn create(&self, auth: &NewDeviceAuth) -> Result<()>;

    /// Decides whether the flow keyed by `handle_hash` may poll the IdP now,
    /// and if so moves its `next_poll_at` forward by its interval in the same
    /// step -- two concurrent polls of one flow claim it at most once.
    async fn claim_poll(&self, handle_hash: &str) -> Result<Claim>;

    /// Grows the flow's interval by [`SLOW_DOWN_STEP_SECS`] and pushes its
    /// next permitted poll that far from now. Returns the new interval, or
    /// `None` if the flow is gone.
    async fn bump_interval(&self, handle_hash: &str) -> Result<Option<u32>>;

    /// Removes the flow and returns it: a flow ends at most once.
    async fn consume(&self, handle_hash: &str) -> Result<Option<DeviceAuth>>;
}

/// Hashes a device handle for storage and lookup. The handle itself is never
/// persisted -- the same rule as a session token.
#[must_use]
pub fn hash_handle(handle: &str) -> String {
    crate::utils::hex::encode_lower(&Sha256::digest(handle.as_bytes()))
}

/// A fresh opaque device handle: 32 random bytes, URL-safe base64.
#[must_use]
pub fn generate_handle() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::Rng;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `secs` as a chrono duration. The interval is bounded well inside `i64`.
fn seconds(secs: u64) -> chrono::Duration {
    chrono::Duration::seconds(i64::try_from(secs).unwrap_or(i64::MAX))
}

/// Postgres-backed device-login store.
#[derive(Debug, Clone)]
pub struct SqlDeviceAuthStore {
    db: Arc<DatabaseConnection>,
    /// Source of every stamp and comparison. Injected so the pacing and the
    /// expiry can be driven by advancing a clock instead of by sleeping.
    clock: Arc<dyn Clock>,
}

impl SqlDeviceAuthStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

fn to_device_auth(model: beam_entity::device_auth::Model) -> DeviceAuth {
    let beam_entity::device_auth::Model {
        handle_hash,
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        interval_secs,
        next_poll_at,
        created_at,
        expires_at,
    } = model;
    DeviceAuth {
        handle_hash,
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        interval_secs: u32::try_from(interval_secs).unwrap_or(0),
        next_poll_at: next_poll_at.with_timezone(&Utc),
        created_at: created_at.with_timezone(&Utc),
        expires_at: expires_at.with_timezone(&Utc),
    }
}

/// Stored intervals are written from a `u32` and only ever grown by a small
/// step, so this never saturates in practice; saturating keeps a corrupt row
/// from wrapping.
fn stored_interval(interval_secs: u32) -> i32 {
    i32::try_from(interval_secs).unwrap_or(i32::MAX)
}

#[async_trait]
impl DeviceAuthStore for SqlDeviceAuthStore {
    async fn create(&self, auth: &NewDeviceAuth) -> Result<()> {
        let now = self.clock.now();

        DeviceAuthEntity::delete_many()
            .filter(Column::ExpiresAt.lt(now))
            .exec(self.db.as_ref())
            .await?;

        let NewDeviceAuth {
            handle_hash,
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
            interval_secs,
            expires_in_secs,
        } = auth.clone();
        DeviceAuthActiveModel {
            handle_hash: Set(handle_hash),
            device_code: Set(device_code),
            user_code: Set(user_code),
            verification_uri: Set(verification_uri),
            verification_uri_complete: Set(verification_uri_complete),
            interval_secs: Set(stored_interval(interval_secs)),
            next_poll_at: Set(now.into()),
            created_at: Set(now.into()),
            expires_at: Set((now + seconds(expires_in_secs)).into()),
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(())
    }

    async fn claim_poll(&self, handle_hash: &str) -> Result<Claim> {
        let now = self.clock.now();
        let Some(model) = DeviceAuthEntity::find_by_id(handle_hash.to_owned())
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(Claim::NotFound);
        };
        let auth = to_device_auth(model);

        if auth.expires_at <= now {
            return Ok(Claim::Expired);
        }
        if auth.next_poll_at > now {
            return Ok(Claim::TooEarly {
                interval_secs: auth.interval_secs,
            });
        }

        // The claim itself: conditional on the row still being due, so of two
        // polls that both read it as due only the first UPDATE matches -- the
        // second re-evaluates `next_poll_at` against the committed row under
        // READ COMMITTED and changes nothing.
        let next_poll_at = now + seconds(u64::from(auth.interval_secs));
        let claimed = DeviceAuthEntity::update_many()
            .col_expr(
                Column::NextPollAt,
                Expr::value(sea_orm::Value::from(
                    chrono::DateTime::<chrono::FixedOffset>::from(next_poll_at),
                )),
            )
            .filter(Column::HandleHash.eq(handle_hash))
            .filter(Column::NextPollAt.lte(now))
            .exec(self.db.as_ref())
            .await?;

        if claimed.rows_affected == 0 {
            return Ok(Claim::TooEarly {
                interval_secs: auth.interval_secs,
            });
        }
        Ok(Claim::Claimed(DeviceAuth {
            next_poll_at,
            ..auth
        }))
    }

    async fn bump_interval(&self, handle_hash: &str) -> Result<Option<u32>> {
        let now = self.clock.now();
        let Some(model) = DeviceAuthEntity::find_by_id(handle_hash.to_owned())
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };
        let interval_secs = u32::try_from(model.interval_secs)
            .unwrap_or(0)
            .saturating_add(SLOW_DOWN_STEP_SECS);
        let updated = DeviceAuthEntity::update_many()
            .col_expr(
                Column::IntervalSecs,
                Expr::value(stored_interval(interval_secs)),
            )
            .col_expr(
                Column::NextPollAt,
                Expr::value(sea_orm::Value::from(
                    chrono::DateTime::<chrono::FixedOffset>::from(
                        now + seconds(u64::from(interval_secs)),
                    ),
                )),
            )
            .filter(Column::HandleHash.eq(handle_hash))
            .exec(self.db.as_ref())
            .await?;
        Ok((updated.rows_affected > 0).then_some(interval_secs))
    }

    async fn consume(&self, handle_hash: &str) -> Result<Option<DeviceAuth>> {
        // One `DELETE ... RETURNING`, for the reason `SqlPendingAuthStore`
        // gives: a SELECT-then-DELETE lets two callers both end one flow --
        // here, both mint a session from one approval.
        let deleted = DeviceAuthEntity::delete_by_id(handle_hash.to_owned())
            .exec_with_returning(self.db.as_ref())
            .await?;
        Ok(deleted.map(to_device_auth))
    }
}

/// In-memory device-login store for tests.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct InMemoryDeviceAuthStore {
        entries: Mutex<HashMap<String, DeviceAuth>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryDeviceAuthStore {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                entries: Mutex::new(HashMap::new()),
                clock,
            }
        }

        /// Every stored flow's handle hash, for asserting what was persisted.
        pub fn handle_hashes(&self) -> Vec<String> {
            self.entries.lock().unwrap().keys().cloned().collect()
        }
    }

    impl Default for InMemoryDeviceAuthStore {
        fn default() -> Self {
            Self::new(Arc::new(RealClock))
        }
    }

    #[async_trait]
    impl DeviceAuthStore for InMemoryDeviceAuthStore {
        async fn create(&self, auth: &NewDeviceAuth) -> Result<()> {
            let now = self.clock.now();
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|_, stored| stored.expires_at >= now);
            entries.insert(
                auth.handle_hash.clone(),
                DeviceAuth {
                    handle_hash: auth.handle_hash.clone(),
                    device_code: auth.device_code.clone(),
                    user_code: auth.user_code.clone(),
                    verification_uri: auth.verification_uri.clone(),
                    verification_uri_complete: auth.verification_uri_complete.clone(),
                    interval_secs: auth.interval_secs,
                    next_poll_at: now,
                    created_at: now,
                    expires_at: now + seconds(auth.expires_in_secs),
                },
            );
            Ok(())
        }

        async fn claim_poll(&self, handle_hash: &str) -> Result<Claim> {
            let now = self.clock.now();
            let mut entries = self.entries.lock().unwrap();
            let Some(auth) = entries.get_mut(handle_hash) else {
                return Ok(Claim::NotFound);
            };
            if auth.expires_at <= now {
                return Ok(Claim::Expired);
            }
            if auth.next_poll_at > now {
                return Ok(Claim::TooEarly {
                    interval_secs: auth.interval_secs,
                });
            }
            auth.next_poll_at = now + seconds(u64::from(auth.interval_secs));
            Ok(Claim::Claimed(auth.clone()))
        }

        async fn bump_interval(&self, handle_hash: &str) -> Result<Option<u32>> {
            let now = self.clock.now();
            let mut entries = self.entries.lock().unwrap();
            let Some(auth) = entries.get_mut(handle_hash) else {
                return Ok(None);
            };
            auth.interval_secs = auth.interval_secs.saturating_add(SLOW_DOWN_STEP_SECS);
            auth.next_poll_at = now + seconds(u64::from(auth.interval_secs));
            Ok(Some(auth.interval_secs))
        }

        async fn consume(&self, handle_hash: &str) -> Result<Option<DeviceAuth>> {
            Ok(self.entries.lock().unwrap().remove(handle_hash))
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use beam_domain::services::TestClock;

    use super::DeviceAuthStore;
    use super::in_memory::InMemoryDeviceAuthStore;
    use crate::utils::contract::fixture::DeviceAuthStoreFixture;

    /// The hermetic instantiation of the shared `DeviceAuthStore` contract.
    pub struct InMemoryFixture {
        store: InMemoryDeviceAuthStore,
        clock: Arc<TestClock>,
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                store: InMemoryDeviceAuthStore::new(clock.clone()),
                clock,
            }
        }
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl DeviceAuthStoreFixture for InMemoryFixture {
        fn store(&self) -> &dyn DeviceAuthStore {
            &self.store
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::new()
    }

    crate::device_auth_store_contract!(setup);
}

#[cfg(test)]
mod handle_tests {
    use super::{generate_handle, hash_handle};

    #[test]
    fn a_handle_carries_256_bits_and_never_repeats() {
        let a = generate_handle();
        let b = generate_handle();
        // 32 bytes of URL-safe, unpadded base64 is 43 characters.
        assert_eq!(a.len(), 43, "{a}");
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "a handle travels in JSON and logs unescaped: {a}"
        );
    }

    #[test]
    fn the_stored_hash_is_the_sha256_of_the_handle_not_the_handle() {
        // The published SHA-256 test vector for "abc" (FIPS 180-2).
        assert_eq!(
            hash_handle("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
