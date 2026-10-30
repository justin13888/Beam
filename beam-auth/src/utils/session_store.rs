use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, DeleteResult, EntityTrait,
    QueryFilter,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;
use uuid::Uuid;

use beam_domain::services::{Clock, RealClock};
use beam_entity::session::{ActiveModel as SessionActiveModel, Column, Entity as SessionEntity};

// A session token is an opaque credential: minted fresh, and persisted only
// as its hash.
use crate::utils::opaque_token::{generate as generate_session_token, hash as hash_token, seconds};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionData {
    pub user_id: String,
    pub device_hash: String,
    pub ip: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Database error: {0}")]
    Db(#[from] sea_orm::DbErr),
    /// A user id is not a UUID.
    ///
    /// Only reachable through a bug: user ids come from authenticated
    /// sessions, and session ids are typed `Uuid` from the path onwards.
    #[error("Invalid id: {0}")]
    InvalidId(#[from] uuid::Error),
}

type Result<T> = std::result::Result<T, SessionError>;

/// How long a session's idle expiry must go untouched before `get_and_touch`
/// bothers sliding it forward again. Avoids a write on every single
/// authenticated request while still keeping the idle window meaningfully
/// accurate.
pub const SESSION_TOUCH_THROTTLE_SECS: i64 = 3600;

/// Resolves a presented session token to its [`SessionData`], sliding the
/// idle expiry forward (throttled per [`SESSION_TOUCH_THROTTLE_SECS`]) if the
/// session is still valid. Shared by every caller that authenticates a
/// request via the `beam_session` cookie -- REST route guards and the OIDC
/// `/me`/`/logout`/`/sessions` handlers alike -- so the touch-throttling
/// policy lives in exactly one place.
pub async fn get_and_touch(
    store: &dyn SessionStore,
    token: &str,
    idle_ttl_secs: u64,
) -> Result<Option<SessionData>> {
    let Some(session) = store.get(token).await? else {
        return Ok(None);
    };

    if store.now().timestamp() - session.last_active > SESSION_TOUCH_THROTTLE_SECS {
        let _ = store.touch(token, idle_ttl_secs).await;
    }

    Ok(Some(session))
}

/// A thread-safe, asynchronous store for managing user sessions.
///
/// This trait abstracts the underlying storage mechanism (Postgres, or an
/// in-memory map for tests) used to persist session data across requests.
/// Every session is minted by an OIDC login (ADR-0003), through one of two
/// flows that share this store: the browser's authorization-code flow, whose
/// token reaches the client as the session cookie, and the device-
/// authorization grant (RFC 8628) a TV or phone client completes, whose
/// token it receives in the response body and then presents as that same
/// cookie (ADR-0017). Both pass the deployment's configured idle and
/// absolute TTLs.
///
/// The token returned by `create` and accepted by `get`/`touch`/`delete` is
/// always the plaintext opaque credential; only its SHA-256 hash is ever
/// persisted; see `beam_entity::session::Model`.
#[async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    /// Persists new session data and returns the plaintext opaque token the
    /// caller should hand to the client (as the session cookie, or in the
    /// device grant's response). `created_at` and `last_active` are stamped
    /// from the store's clock, whatever `data` carries. A `user_id` that is
    /// not a UUID is refused before anything is written.
    ///
    /// # Parameters
    /// - `data`: The session state to be stored.
    /// - `idle_ttl_secs`: How long the session survives without activity;
    ///   `touch` slides this forward, capped by `absolute_ttl_secs`.
    /// - `absolute_ttl_secs`: Hard ceiling on session lifetime from creation,
    ///   regardless of activity.
    async fn create(
        &self,
        data: &SessionData,
        idle_ttl_secs: u64,
        absolute_ttl_secs: u64,
    ) -> Result<String>;

    /// Retrieves session data for the session identified by the presented
    /// plaintext token.
    ///
    /// # Returns
    /// - `Ok(Some(SessionData))` if the session exists and has not expired
    ///   (idle or absolute).
    /// - `Ok(None)` if the session is not found or is expired.
    /// - `Err` if a storage backend error occurs.
    async fn get(&self, token: &str) -> Result<Option<SessionData>>;

    /// Slides the idle expiry forward by `idle_ttl_secs` from now, never
    /// extending past the session's absolute expiry ceiling.
    ///
    /// This is typically called on every request to keep the user's session
    /// active.
    ///
    /// # Errors
    /// Returns an error if the store is unreachable. A missing session is
    /// not an error -- there is nothing to touch.
    async fn touch(&self, token: &str, idle_ttl_secs: u64) -> Result<()>;

    /// Immediately invalidates and removes the session identified by the
    /// presented plaintext token.
    async fn delete(&self, token: &str) -> Result<()>;

    /// Invalidates all active sessions associated with a specific user.
    ///
    /// This is useful for "log out of all devices" functionality or security revocations.
    ///
    /// # Returns
    /// The number of sessions successfully deleted.
    async fn delete_all_for_user(&self, user_id: &str) -> Result<u64>;

    /// Returns a list of all active sessions belonging to a specific user.
    ///
    /// Each entry is `(id, SessionData)`, where `id` is the session's stable
    /// internal identifier -- NOT the credential itself, which cannot be
    /// recovered once hashed. Use `delete_by_id` to revoke one of these.
    async fn list_for_user(&self, user_id: &str) -> Result<Vec<(Uuid, SessionData)>>;

    /// Revokes a specific session by its internal id, scoped to the given
    /// owning user (so one user can never revoke another's session by
    /// guessing an id). Returns `true` if a session was deleted.
    async fn delete_by_id(&self, id: Uuid, user_id: &str) -> Result<bool>;

    /// The store's own view of the current time.
    ///
    /// Every expiry this store enforces is relative to it, so a caller that
    /// needs to reason about the same timeline -- notably
    /// [`get_and_touch`]'s throttle -- must read it here rather than from the
    /// wall clock, or the two disagree whenever the clock is injected.
    fn now(&self) -> chrono::DateTime<Utc>;
}

fn to_session_data(model: &beam_entity::session::Model) -> SessionData {
    SessionData {
        user_id: model.user_id.to_string(),
        device_hash: model.device_hash.clone(),
        ip: model.ip.clone(),
        created_at: model.created_at.timestamp(),
        last_active: model.last_active.timestamp(),
    }
}

/// Postgres-backed session store.
///
/// Sessions carry their own idle/absolute expiry; this store filters expired
/// rows out of reads rather than deleting them proactively (there is no
/// background sweep here yet -- a periodic cleanup task is future work, not
/// required for correctness since every read already filters on both
/// expiries).
#[derive(Debug, Clone)]
pub struct PgSessionStore {
    db: Arc<DatabaseConnection>,
    /// Source of every timestamp and expiry this store writes or compares.
    /// Injected so the idle/absolute TTL rules can be driven by advancing a
    /// clock instead of by sleeping for the length of a session.
    clock: Arc<dyn Clock>,
}

impl PgSessionStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.clock.now()
    }

    async fn create(
        &self,
        data: &SessionData,
        idle_ttl_secs: u64,
        absolute_ttl_secs: u64,
    ) -> Result<String> {
        let token = generate_session_token();
        let now = self.clock.now();
        let idle_expires_at = now + seconds(idle_ttl_secs);
        let absolute_expires_at = now + seconds(absolute_ttl_secs);

        let active_model = SessionActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(data.user_id.parse()?),
            token_hash: Set(hash_token(&token)),
            device_hash: Set(data.device_hash.clone()),
            ip: Set(data.ip.clone()),
            created_at: Set(now.into()),
            last_active: Set(now.into()),
            idle_expires_at: Set(idle_expires_at.into()),
            absolute_expires_at: Set(absolute_expires_at.into()),
        };
        active_model.insert(self.db.as_ref()).await?;

        debug!("Created session for user {}", data.user_id);
        Ok(token)
    }

    async fn get(&self, token: &str) -> Result<Option<SessionData>> {
        let Some(model) = SessionEntity::find()
            .filter(Column::TokenHash.eq(hash_token(token)))
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };

        let now = self.clock.now();
        if model.idle_expires_at < now || model.absolute_expires_at < now {
            return Ok(None);
        }

        Ok(Some(to_session_data(&model)))
    }

    async fn touch(&self, token: &str, idle_ttl_secs: u64) -> Result<()> {
        let Some(model) = SessionEntity::find()
            .filter(Column::TokenHash.eq(hash_token(token)))
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };

        let now = self.clock.now();
        let requested_idle_expiry = now + seconds(idle_ttl_secs);
        // Never slide the idle deadline past the absolute ceiling.
        let idle_expires_at = requested_idle_expiry.min(model.absolute_expires_at.into());
        let mut active_model: SessionActiveModel = model.into();
        active_model.last_active = Set(now.into());
        active_model.idle_expires_at = Set(idle_expires_at.into());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    async fn delete(&self, token: &str) -> Result<()> {
        SessionEntity::delete_many()
            .filter(Column::TokenHash.eq(hash_token(token)))
            .exec(self.db.as_ref())
            .await?;
        debug!("Deleted session");
        Ok(())
    }

    async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
        let user_uuid: Uuid = user_id.parse()?;
        let result: DeleteResult = SessionEntity::delete_many()
            .filter(Column::UserId.eq(user_uuid))
            .exec(self.db.as_ref())
            .await?;

        debug!(
            "Deleted all {} sessions for user {}",
            result.rows_affected, user_id
        );
        Ok(result.rows_affected)
    }

    async fn list_for_user(&self, user_id: &str) -> Result<Vec<(Uuid, SessionData)>> {
        let user_uuid: Uuid = user_id.parse()?;
        let now = self.clock.now();
        let models = SessionEntity::find()
            .filter(Column::UserId.eq(user_uuid))
            .filter(Column::IdleExpiresAt.gt(now))
            .filter(Column::AbsoluteExpiresAt.gt(now))
            .all(self.db.as_ref())
            .await?;

        Ok(models.iter().map(|m| (m.id, to_session_data(m))).collect())
    }

    async fn delete_by_id(&self, id: Uuid, user_id: &str) -> Result<bool> {
        let user_uuid: Uuid = user_id.parse()?;
        let result: DeleteResult = SessionEntity::delete_many()
            .filter(Column::Id.eq(id))
            .filter(Column::UserId.eq(user_uuid))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected > 0)
    }
}

/// Builders for tests, beside the type they build.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::SessionData;

    /// A session for `user_id` from one fixed device and address. The
    /// `created_at`/`last_active` stamps are zero: every store stamps its own
    /// and ignores the caller's.
    #[must_use]
    pub fn session_for(user_id: uuid::Uuid) -> SessionData {
        SessionData {
            user_id: user_id.to_string(),
            device_hash: "device-hash".to_owned(),
            ip: "203.0.113.7".to_owned(),
            created_at: 0,
            last_active: 0,
        }
    }
}

/// In-memory session store for use in tests and offline scenarios.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Clone)]
    struct StoredSession {
        id: Uuid,
        data: SessionData,
        idle_expires_at: chrono::DateTime<Utc>,
        absolute_expires_at: chrono::DateTime<Utc>,
    }

    #[derive(Debug)]
    pub struct InMemorySessionStore {
        // Keyed by hashed token, matching the Postgres store's lookup shape.
        sessions: Mutex<HashMap<String, StoredSession>>,
        clock: Arc<dyn Clock>,
    }

    impl Default for InMemorySessionStore {
        fn default() -> Self {
            Self::new(Arc::new(RealClock))
        }
    }

    impl InMemorySessionStore {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                sessions: Mutex::new(HashMap::new()),
                clock,
            }
        }

        /// Locks the session map, recovering from a poisoned lock rather
        /// than panicking -- one panicked holder must not permanently take
        /// down every session operation. The map is consistent after every
        /// individual mutation, so recovered data is safe to reuse.
        fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, StoredSession>> {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    #[async_trait]
    impl SessionStore for InMemorySessionStore {
        fn now(&self) -> chrono::DateTime<Utc> {
            self.clock.now()
        }

        async fn create(
            &self,
            data: &SessionData,
            idle_ttl_secs: u64,
            absolute_ttl_secs: u64,
        ) -> Result<String> {
            // Parsed, not stored as given: the SQL store binds a `Uuid`, so a
            // malformed id is `InvalidId` there before anything is written.
            let _: Uuid = data.user_id.parse()?;
            let token = generate_session_token();
            let now = self.clock.now();
            // `created_at`/`last_active` belong to the store, not the caller:
            // the Postgres store stamps them from its own clock and ignores
            // whatever the caller put in the struct. Echoing the caller's
            // values back here instead made the two disagree.
            let data = SessionData {
                created_at: now.timestamp(),
                last_active: now.timestamp(),
                ..data.clone()
            };
            self.lock_sessions().insert(
                hash_token(&token),
                StoredSession {
                    id: Uuid::new_v4(),
                    data,
                    idle_expires_at: now + seconds(idle_ttl_secs),
                    absolute_expires_at: now + seconds(absolute_ttl_secs),
                },
            );
            Ok(token)
        }

        async fn get(&self, token: &str) -> Result<Option<SessionData>> {
            let now = self.clock.now();
            Ok(self
                .sessions
                .lock()
                .unwrap()
                .get(&hash_token(token))
                .filter(|s| s.idle_expires_at >= now && s.absolute_expires_at >= now)
                .map(|s| s.data.clone()))
        }

        async fn touch(&self, token: &str, idle_ttl_secs: u64) -> Result<()> {
            let now = self.clock.now();
            if let Some(session) = self.lock_sessions().get_mut(&hash_token(token)) {
                session.data.last_active = now.timestamp();
                session.idle_expires_at =
                    (now + seconds(idle_ttl_secs)).min(session.absolute_expires_at);
            }
            Ok(())
        }

        async fn delete(&self, token: &str) -> Result<()> {
            self.lock_sessions().remove(&hash_token(token));
            Ok(())
        }

        async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
            // As in `delete_by_id`: the SQL store parses the id first.
            let _: Uuid = user_id.parse()?;
            let mut sessions = self.lock_sessions();
            let to_remove: Vec<String> = sessions
                .iter()
                .filter(|(_, v)| v.data.user_id == user_id)
                .map(|(k, _)| k.clone())
                .collect();
            let count = to_remove.len() as u64;
            for key in to_remove {
                sessions.remove(&key);
            }
            Ok(count)
        }

        async fn list_for_user(&self, user_id: &str) -> Result<Vec<(Uuid, SessionData)>> {
            // Expired sessions are filtered out here, matching the Postgres
            // store: this list is what the profile page offers for revocation,
            // and an already-dead session in it is a confusing no-op button.
            let _: Uuid = user_id.parse()?;
            let now = self.clock.now();
            let sessions = self.lock_sessions();
            Ok(sessions
                .values()
                .filter(|s| {
                    s.data.user_id == user_id
                        && s.idle_expires_at > now
                        && s.absolute_expires_at > now
                })
                .map(|s| (s.id, s.data.clone()))
                .collect())
        }

        async fn delete_by_id(&self, id: Uuid, user_id: &str) -> Result<bool> {
            // The user id is parsed, not string-compared: the SQL store parses
            // it before it queries, so a malformed one is an error there too.
            let _: Uuid = user_id.parse()?;
            let mut sessions = self.lock_sessions();
            let key = sessions
                .iter()
                .find(|(_, s)| s.id == id && s.data.user_id == user_id)
                .map(|(k, _)| k.clone());
            match key {
                Some(key) => {
                    sessions.remove(&key);
                    Ok(true)
                }
                None => Ok(false),
            }
        }
    }

    /// One [`SessionStore`] method, for [`FailingSessionStore`] to fail.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionStoreOperation {
        Create,
        Get,
        Touch,
        Delete,
        DeleteAllForUser,
        ListForUser,
        DeleteById,
    }

    /// An [`InMemorySessionStore`] whose one chosen method fails as an
    /// unreachable database would, so a caller's storage-failure path can be
    /// driven without one. Every other method behaves as the store it wraps.
    #[derive(Debug)]
    pub struct FailingSessionStore {
        inner: InMemorySessionStore,
        failing: SessionStoreOperation,
    }

    impl FailingSessionStore {
        pub fn new(inner: InMemorySessionStore, failing: SessionStoreOperation) -> Self {
            Self { inner, failing }
        }

        /// The wrapped store, for seeding and for asserting what was written.
        pub fn inner(&self) -> &InMemorySessionStore {
            &self.inner
        }

        fn check(&self, operation: SessionStoreOperation) -> Result<()> {
            if operation == self.failing {
                return Err(SessionError::Db(sea_orm::DbErr::Custom(format!(
                    "injected failure in {operation:?}"
                ))));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl SessionStore for FailingSessionStore {
        fn now(&self) -> chrono::DateTime<Utc> {
            self.inner.now()
        }

        async fn create(
            &self,
            data: &SessionData,
            idle_ttl_secs: u64,
            absolute_ttl_secs: u64,
        ) -> Result<String> {
            self.check(SessionStoreOperation::Create)?;
            self.inner
                .create(data, idle_ttl_secs, absolute_ttl_secs)
                .await
        }

        async fn get(&self, token: &str) -> Result<Option<SessionData>> {
            self.check(SessionStoreOperation::Get)?;
            self.inner.get(token).await
        }

        async fn touch(&self, token: &str, idle_ttl_secs: u64) -> Result<()> {
            self.check(SessionStoreOperation::Touch)?;
            self.inner.touch(token, idle_ttl_secs).await
        }

        async fn delete(&self, token: &str) -> Result<()> {
            self.check(SessionStoreOperation::Delete)?;
            self.inner.delete(token).await
        }

        async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
            self.check(SessionStoreOperation::DeleteAllForUser)?;
            self.inner.delete_all_for_user(user_id).await
        }

        async fn list_for_user(&self, user_id: &str) -> Result<Vec<(Uuid, SessionData)>> {
            self.check(SessionStoreOperation::ListForUser)?;
            self.inner.list_for_user(user_id).await
        }

        async fn delete_by_id(&self, id: Uuid, user_id: &str) -> Result<bool> {
            self.check(SessionStoreOperation::DeleteById)?;
            self.inner.delete_by_id(id, user_id).await
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use beam_domain::services::TestClock;

    use super::SessionStore;
    use super::in_memory::InMemorySessionStore;
    use crate::utils::contract::fixture::SessionStoreFixture;

    /// The hermetic instantiation of the shared `SessionStore` contract.
    pub struct InMemoryFixture {
        store: InMemorySessionStore,
        clock: Arc<TestClock>,
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            // Deliberately not the epoch. `get_and_touch` computes
            // `now - last_active`, and at time zero that is indistinguishable
            // from `now + last_active` -- a mutated `+` would pass every test.
            let clock = Arc::new(TestClock::starting_at(
                chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
            ));
            Self {
                store: InMemorySessionStore::new(clock.clone()),
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
    impl SessionStoreFixture for InMemoryFixture {
        fn store(&self) -> &dyn SessionStore {
            &self.store
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }

        async fn new_user(&self) -> uuid::Uuid {
            uuid::Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::new()
    }

    crate::session_store_contract!(setup);
}

/// [`get_and_touch`] when the store behind it fails.
#[cfg(test)]
mod get_and_touch_under_a_failing_store {
    use std::sync::Arc;
    use std::time::Duration;

    use beam_domain::services::TestClock;

    use super::in_memory::{FailingSessionStore, InMemorySessionStore, SessionStoreOperation};
    use super::test_utils::session_for;
    use super::{SESSION_TOUCH_THROTTLE_SECS, SessionError, SessionStore, get_and_touch};

    const IDLE_TTL: u64 = 14 * 24 * 3600;

    /// A store failing `failing`, holding one live session, with the clock
    /// already past the touch throttle so `get_and_touch` reaches `touch`.
    async fn store_with_a_session_due_a_touch(
        failing: SessionStoreOperation,
    ) -> (FailingSessionStore, String) {
        let clock = Arc::new(TestClock::starting_at(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
        ));
        let store = FailingSessionStore::new(InMemorySessionStore::new(clock.clone()), failing);
        let token = store
            .inner()
            .create(&session_for(uuid::Uuid::new_v4()), IDLE_TTL, 2 * IDLE_TTL)
            .await
            .unwrap();
        clock.advance(Duration::from_secs(SESSION_TOUCH_THROTTLE_SECS as u64 + 1));
        (store, token)
    }

    #[tokio::test]
    async fn a_store_that_cannot_be_read_is_an_error_not_a_signed_out_request() {
        // `Ok(None)` would answer 401 and send the user to log in again over
        // a database outage; the caller has to be able to tell the two apart.
        let (store, token) = store_with_a_session_due_a_touch(SessionStoreOperation::Get).await;

        let resolved = get_and_touch(&store, &token, IDLE_TTL).await;

        assert!(matches!(resolved, Err(SessionError::Db(_))), "{resolved:?}");
    }

    #[tokio::test]
    async fn a_failed_touch_still_resolves_the_session() {
        // Sliding the idle window is best-effort bookkeeping: a write that
        // fails must not turn an otherwise valid request away.
        let (store, token) = store_with_a_session_due_a_touch(SessionStoreOperation::Touch).await;

        let resolved = get_and_touch(&store, &token, IDLE_TTL)
            .await
            .expect("a failed touch is swallowed");

        let session = resolved.expect("the session is still valid");
        let stored = store.inner().get(&token).await.unwrap().unwrap();
        assert_eq!(session.user_id, stored.user_id);
        assert_eq!(
            stored.last_active, session.last_active,
            "the touch failed, so nothing was rewritten"
        );
    }
}
