use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, DeleteResult, EntityTrait,
    QueryFilter,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use beam_entity::session::{ActiveModel as SessionActiveModel, Column, Entity as SessionEntity};

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
    #[error("Invalid user ID: {0}")]
    InvalidUserId(#[from] uuid::Error),
}

type Result<T> = std::result::Result<T, SessionError>;

/// A thread-safe, asynchronous store for managing user sessions.
///
/// This trait abstracts the underlying storage mechanism (Postgres, or an
/// in-memory map for tests) used to persist session data across requests.
#[async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    /// Persists new session data and returns a unique session identifier.
    ///
    /// # Parameters
    /// - `data`: The session state to be stored.
    /// - `ttl_secs`: Time-to-live in seconds before the session expires.
    ///
    /// # Returns
    /// A `Result` containing the generated `String` session ID.
    async fn create(&self, data: &SessionData, ttl_secs: u64) -> Result<String>;

    /// Retrieves session data associated with a specific session ID.
    ///
    /// # Returns
    /// - `Ok(Some(SessionData))` if the session exists and has not expired.
    /// - `Ok(None)` if the session is not found or is expired.
    /// - `Err` if a storage backend error occurs.
    async fn get(&self, session_id: &str) -> Result<Option<SessionData>>;

    /// Updates the expiration time (TTL) of an existing session.
    ///
    /// This is typically called on every request to keep the user's session active.
    ///
    /// # Errors
    /// Returns an error if the session does not exist or the store is unreachable.
    async fn touch(&self, session_id: &str, ttl_secs: u64) -> Result<()>;

    /// Immediately invalidates and removes a specific session.
    async fn delete(&self, session_id: &str) -> Result<()>;

    /// Invalidates all active sessions associated with a specific user.
    ///
    /// This is useful for "log out of all devices" functionality or security revocations.
    ///
    /// # Returns
    /// The number of sessions successfully deleted.
    async fn delete_all_for_user(&self, user_id: &str) -> Result<u64>;

    /// Returns a list of all active sessions belonging to a specific user.
    ///
    /// Each entry in the vector is a tuple containing the `(session_id, SessionData)`.
    async fn list_for_user(&self, user_id: &str) -> Result<Vec<(String, SessionData)>>;
}

/// Generates an opaque, random 32-byte URL-safe base64 session identifier.
fn generate_session_id() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::Rng;

    let mut bytes = [0u8; 32];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut bytes);

    URL_SAFE_NO_PAD.encode(bytes)
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
/// Sessions carry their own `expires_at`; this store filters expired rows out
/// of reads rather than deleting them proactively (there is no background
/// sweep here yet -- unlike Redis, Postgres does not expire rows on its own).
/// A periodic cleanup task belongs with the broader session-model work
/// tracked in ADR-0005/ADR-0003, not this storage-backend swap.
#[derive(Debug, Clone)]
pub struct PgSessionStore {
    db: DatabaseConnection,
}

impl PgSessionStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, data: &SessionData, ttl_secs: u64) -> Result<String> {
        let session_id = generate_session_id();
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_secs as i64);

        let active_model = SessionActiveModel {
            id: Set(session_id.clone()),
            user_id: Set(data.user_id.parse()?),
            device_hash: Set(data.device_hash.clone()),
            ip: Set(data.ip.clone()),
            created_at: Set(now.into()),
            last_active: Set(now.into()),
            expires_at: Set(expires_at.into()),
        };
        active_model.insert(&self.db).await?;

        debug!("Created session {} for user {}", session_id, data.user_id);
        Ok(session_id)
    }

    async fn get(&self, session_id: &str) -> Result<Option<SessionData>> {
        let Some(model) = SessionEntity::find_by_id(session_id.to_string())
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };

        if model.expires_at < Utc::now() {
            return Ok(None);
        }

        Ok(Some(to_session_data(&model)))
    }

    async fn touch(&self, session_id: &str, ttl_secs: u64) -> Result<()> {
        let Some(model) = SessionEntity::find_by_id(session_id.to_string())
            .one(&self.db)
            .await?
        else {
            return Ok(());
        };

        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_secs as i64);
        let mut active_model: SessionActiveModel = model.into();
        active_model.last_active = Set(now.into());
        active_model.expires_at = Set(expires_at.into());
        active_model.update(&self.db).await?;

        Ok(())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        SessionEntity::delete_by_id(session_id.to_string())
            .exec(&self.db)
            .await?;
        debug!("Deleted session {}", session_id);
        Ok(())
    }

    async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
        let user_uuid: uuid::Uuid = user_id.parse()?;
        let result: DeleteResult = SessionEntity::delete_many()
            .filter(Column::UserId.eq(user_uuid))
            .exec(&self.db)
            .await?;

        debug!(
            "Deleted all {} sessions for user {}",
            result.rows_affected, user_id
        );
        Ok(result.rows_affected)
    }

    async fn list_for_user(&self, user_id: &str) -> Result<Vec<(String, SessionData)>> {
        let user_uuid: uuid::Uuid = user_id.parse()?;
        let models = SessionEntity::find()
            .filter(Column::UserId.eq(user_uuid))
            .filter(Column::ExpiresAt.gt(Utc::now()))
            .all(&self.db)
            .await?;

        Ok(models
            .iter()
            .map(|m| (m.id.clone(), to_session_data(m)))
            .collect())
    }
}

/// In-memory session store for use in tests and offline scenarios.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use uuid::Uuid;

    #[derive(Debug, Default)]
    pub struct InMemorySessionStore {
        sessions: Mutex<HashMap<String, SessionData>>,
    }

    #[async_trait]
    impl SessionStore for InMemorySessionStore {
        async fn create(&self, data: &SessionData, _ttl_secs: u64) -> Result<String> {
            let session_id = Uuid::new_v4().to_string();
            self.sessions
                .lock()
                .unwrap()
                .insert(session_id.clone(), data.clone());
            Ok(session_id)
        }

        async fn get(&self, session_id: &str) -> Result<Option<SessionData>> {
            Ok(self.sessions.lock().unwrap().get(session_id).cloned())
        }

        async fn touch(&self, _session_id: &str, _ttl_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn delete(&self, session_id: &str) -> Result<()> {
            self.sessions.lock().unwrap().remove(session_id);
            Ok(())
        }

        async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
            let mut sessions = self.sessions.lock().unwrap();
            let to_remove: Vec<String> = sessions
                .iter()
                .filter(|(_, v)| v.user_id == user_id)
                .map(|(k, _)| k.clone())
                .collect();
            let count = to_remove.len() as u64;
            for id in to_remove {
                sessions.remove(&id);
            }
            Ok(count)
        }

        async fn list_for_user(&self, user_id: &str) -> Result<Vec<(String, SessionData)>> {
            let sessions = self.sessions.lock().unwrap();
            Ok(sessions
                .iter()
                .filter(|(_, v)| v.user_id == user_id)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
    }
}
