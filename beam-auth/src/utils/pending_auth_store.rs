//! Single-use storage for an in-flight OIDC authorization round-trip: the
//! `state`/`nonce`/PKCE verifier minted by `begin_auth`, looked up and
//! atomically consumed (deleted) when the callback presents its `state`.
//! A `state` value that has already been consumed -- or never existed --
//! can never be exchanged again, which is what makes replaying a captured
//! callback URL harmless.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};

use beam_domain::services::{Clock, RealClock};
use thiserror::Error;

use beam_entity::pending_auth::{
    ActiveModel as PendingAuthActiveModel, Entity as PendingAuthEntity,
};

#[derive(Debug, Clone)]
pub struct PendingAuth {
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub redirect_path: Option<String>,
}

#[derive(Debug, Error)]
pub enum PendingAuthError {
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

type Result<T> = std::result::Result<T, PendingAuthError>;

#[async_trait]
pub trait PendingAuthStore: Send + Sync + std::fmt::Debug {
    /// Persists a new pending authorization, valid for `ttl_secs`.
    async fn create(&self, auth: &PendingAuth, ttl_secs: u64) -> Result<()>;

    /// Atomically looks up and deletes the pending authorization for
    /// `state`, so it can be consumed at most once. Returns `None` if the
    /// state is unknown, already consumed, or expired.
    async fn consume(&self, state: &str) -> Result<Option<PendingAuth>>;
}

#[derive(Debug, Clone)]
pub struct SqlPendingAuthStore {
    db: Arc<DatabaseConnection>,
    /// Source of the creation and expiry stamps. Injected so the TTL rule can
    /// be exercised by advancing a clock instead of waiting one out.
    clock: Arc<dyn Clock>,
}

impl SqlPendingAuthStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

fn to_pending_auth(model: beam_entity::pending_auth::Model) -> PendingAuth {
    PendingAuth {
        state: model.state,
        nonce: model.nonce,
        pkce_verifier: model.pkce_verifier,
        redirect_path: model.redirect_path,
    }
}

#[async_trait]
impl PendingAuthStore for SqlPendingAuthStore {
    async fn create(&self, auth: &PendingAuth, ttl_secs: u64) -> Result<()> {
        let now = self.clock.now();
        let expires_at = now + chrono::Duration::seconds(ttl_secs as i64);

        let active_model = PendingAuthActiveModel {
            state: Set(auth.state.clone()),
            nonce: Set(auth.nonce.clone()),
            pkce_verifier: Set(auth.pkce_verifier.clone()),
            redirect_path: Set(auth.redirect_path.clone()),
            created_at: Set(now.into()),
            expires_at: Set(expires_at.into()),
        };
        active_model.insert(self.db.as_ref()).await?;
        Ok(())
    }

    async fn consume(&self, state: &str) -> Result<Option<PendingAuth>> {
        // Single-use: a single atomic `DELETE ... RETURNING` statement, not
        // a SELECT followed by a separate DELETE. The latter has a TOCTOU
        // race under Postgres's default READ COMMITTED isolation -- two
        // concurrent `consume` calls for the same `state` can both have
        // their SELECT observe the row before either DELETE commits, so
        // both would return `Some` for what's supposed to be a single-use
        // value. `exec_with_returning` deletes and returns the row in one
        // round trip, so at most one caller ever gets `Some` back.
        let mut deleted = PendingAuthEntity::delete_by_id(state.to_string())
            .exec_with_returning(self.db.as_ref())
            .await?;
        let Some(model) = deleted.pop() else {
            return Ok(None);
        };

        if model.expires_at < self.clock.now() {
            return Ok(None);
        }

        Ok(Some(to_pending_auth(model)))
    }
}

/// In-memory pending-auth store for tests.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use chrono::DateTime;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct InMemoryPendingAuthStore {
        entries: Mutex<HashMap<String, (PendingAuth, DateTime<Utc>)>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryPendingAuthStore {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                entries: Mutex::new(HashMap::new()),
                clock,
            }
        }
    }

    impl Default for InMemoryPendingAuthStore {
        fn default() -> Self {
            Self::new(Arc::new(RealClock))
        }
    }

    #[async_trait]
    impl PendingAuthStore for InMemoryPendingAuthStore {
        async fn create(&self, auth: &PendingAuth, ttl_secs: u64) -> Result<()> {
            let expires_at = self.clock.now() + chrono::Duration::seconds(ttl_secs as i64);
            self.entries
                .lock()
                .unwrap()
                .insert(auth.state.clone(), (auth.clone(), expires_at));
            Ok(())
        }

        async fn consume(&self, state: &str) -> Result<Option<PendingAuth>> {
            let now = self.clock.now();
            let removed = self.entries.lock().unwrap().remove(state);
            match removed {
                Some((auth, expires_at)) if expires_at >= now => Ok(Some(auth)),
                _ => Ok(None),
            }
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use beam_domain::services::TestClock;

    use super::PendingAuthStore;
    use super::in_memory::InMemoryPendingAuthStore;
    use crate::utils::contract::fixture::PendingAuthStoreFixture;

    /// The hermetic instantiation of the shared `PendingAuthStore` contract.
    pub struct InMemoryFixture {
        store: InMemoryPendingAuthStore,
        clock: Arc<TestClock>,
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                store: InMemoryPendingAuthStore::new(clock.clone()),
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
    impl PendingAuthStoreFixture for InMemoryFixture {
        fn store(&self) -> &dyn PendingAuthStore {
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

    crate::pending_auth_store_contract!(setup);
}
