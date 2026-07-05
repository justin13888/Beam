//! Single-use storage for an in-flight OIDC authorization round-trip: the
//! `state`/`nonce`/PKCE verifier minted by `begin_auth`, looked up and
//! atomically consumed (deleted) when the callback presents its `state`.
//! A `state` value that has already been consumed -- or never existed --
//! can never be exchanged again, which is what makes replaying a captured
//! callback URL harmless.

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait, TransactionTrait,
};
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
    db: DatabaseConnection,
}

impl SqlPendingAuthStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
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
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_secs as i64);

        let active_model = PendingAuthActiveModel {
            state: Set(auth.state.clone()),
            nonce: Set(auth.nonce.clone()),
            pkce_verifier: Set(auth.pkce_verifier.clone()),
            redirect_path: Set(auth.redirect_path.clone()),
            created_at: Set(now.into()),
            expires_at: Set(expires_at.into()),
        };
        active_model.insert(&self.db).await?;
        Ok(())
    }

    async fn consume(&self, state: &str) -> Result<Option<PendingAuth>> {
        // Single-use: look up and delete within one transaction so a state
        // value can never be raced into being consumed twice.
        let txn = self.db.begin().await?;

        let Some(model) = PendingAuthEntity::find_by_id(state.to_string())
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(None);
        };

        PendingAuthEntity::delete_by_id(state.to_string())
            .exec(&txn)
            .await?;
        txn.commit().await?;

        if model.expires_at < Utc::now() {
            return Ok(None);
        }

        Ok(Some(to_pending_auth(model)))
    }
}

/// In-memory pending-auth store for tests.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use chrono::DateTime;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryPendingAuthStore {
        entries: Mutex<HashMap<String, (PendingAuth, DateTime<Utc>)>>,
    }

    #[async_trait]
    impl PendingAuthStore for InMemoryPendingAuthStore {
        async fn create(&self, auth: &PendingAuth, ttl_secs: u64) -> Result<()> {
            let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
            self.entries
                .lock()
                .unwrap()
                .insert(auth.state.clone(), (auth.clone(), expires_at));
            Ok(())
        }

        async fn consume(&self, state: &str) -> Result<Option<PendingAuth>> {
            let removed = self.entries.lock().unwrap().remove(state);
            match removed {
                Some((auth, expires_at)) if expires_at >= Utc::now() => Ok(Some(auth)),
                _ => Ok(None),
            }
        }
    }
}
