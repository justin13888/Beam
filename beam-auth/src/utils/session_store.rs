use async_trait::async_trait;
use bb8_redis::RedisConnectionManager;
use bb8_redis::bb8::{Pool, PooledConnection};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

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
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("Connection pool error: {0}")]
    Pool(#[from] bb8_redis::bb8::RunError<redis::RedisError>),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, SessionError>;

/// A thread-safe, asynchronous store for managing user sessions.
///
/// This trait abstracts the underlying storage mechanism (e.g., Redis, PostgreSQL,
/// or an in-memory map) used to persist session data across requests.
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

#[derive(Debug)]
pub struct RedisSessionStore {
    pool: Pool<RedisConnectionManager>,
}

impl RedisSessionStore {
    pub async fn new(redis_url: &str) -> Result<Self> {
        let manager = RedisConnectionManager::new(redis_url).map_err(SessionError::Redis)?;
        let pool = Pool::builder()
            .build(manager)
            .await
            .map_err(SessionError::Redis)?;
        Ok(Self { pool })
    }

    async fn get_conn(&self) -> Result<PooledConnection<'_, RedisConnectionManager>> {
        self.pool.get().await.map_err(SessionError::Pool)
    }

    fn session_key(session_id: &str) -> String {
        format!("session:{}", session_id)
    }

    fn user_sessions_key(user_id: &str) -> String {
        format!("user_sessions:{}", user_id)
    }

    fn generate_session_id() -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use rand::Rng;

        let mut bytes = [0u8; 32];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut bytes);

        URL_SAFE_NO_PAD.encode(bytes)
    }
}

#[async_trait]
impl SessionStore for RedisSessionStore {
    async fn create(&self, data: &SessionData, ttl_secs: u64) -> Result<String> {
        let session_id = Self::generate_session_id();
        let key = Self::session_key(&session_id);
        let user_key = Self::user_sessions_key(&data.user_id);
        let value = serde_json::to_string(data)?;

        let mut conn = self.get_conn().await?;

        // Transaction: set session data + add to user set
        let _: () = redis::pipe()
            .atomic()
            .set_ex(&key, &value, ttl_secs)
            .sadd(&user_key, &session_id)
            .query_async(&mut *conn)
            .await?;

        debug!("Created session {} for user {}", session_id, data.user_id);
        Ok(session_id)
    }

    async fn get(&self, session_id: &str) -> Result<Option<SessionData>> {
        let key = Self::session_key(session_id);
        let mut conn = self.get_conn().await?;

        let value: Option<String> = conn.get(&key).await?;

        match value {
            Some(v) => Ok(Some(serde_json::from_str(&v)?)),
            None => Ok(None),
        }
    }

    async fn touch(&self, session_id: &str, ttl_secs: u64) -> Result<()> {
        let key = Self::session_key(session_id);
        let mut conn = self.get_conn().await?;

        let _: bool = conn.expire(&key, ttl_secs as i64).await?;
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        let key = Self::session_key(session_id);
        let mut conn = self.get_conn().await?;

        let value: Option<String> = conn.get(&key).await?;
        if let Some(v) = value {
            if let Ok(data) = serde_json::from_str::<SessionData>(&v) {
                let user_key = Self::user_sessions_key(&data.user_id);
                let _: () = redis::pipe()
                    .atomic()
                    .del(&key)
                    .srem(&user_key, session_id)
                    .query_async(&mut *conn)
                    .await?;
            } else {
                let _: () = conn.del(&key).await?;
            }
        }

        debug!("Deleted session {}", session_id);
        Ok(())
    }

    async fn delete_all_for_user(&self, user_id: &str) -> Result<u64> {
        let user_key = Self::user_sessions_key(user_id);
        let mut conn = self.get_conn().await?;

        let session_ids: Vec<String> = conn.smembers(&user_key).await?;
        if session_ids.is_empty() {
            return Ok(0);
        }

        let mut pipe = redis::pipe();
        pipe.atomic();

        for id in &session_ids {
            pipe.del(Self::session_key(id));
        }
        pipe.del(&user_key);

        let _: () = pipe.query_async(&mut *conn).await?;

        debug!(
            "Deleted all {} sessions for user {}",
            session_ids.len(),
            user_id
        );
        Ok(session_ids.len() as u64)
    }

    async fn list_for_user(&self, user_id: &str) -> Result<Vec<(String, SessionData)>> {
        let user_key = Self::user_sessions_key(user_id);
        let mut conn = self.get_conn().await?;

        let session_ids: Vec<String> = conn.smembers(&user_key).await?;
        let mut sessions = Vec::new();

        for id in session_ids {
            let key = Self::session_key(&id);
            let value: Option<String> = conn.get(&key).await?;

            if let Some(v) = value {
                if let Ok(data) = serde_json::from_str::<SessionData>(&v) {
                    sessions.push((id, data));
                }
            } else {
                // Session expired but still in set - clean it up
                let _: () = conn.srem(&user_key, &id).await?;
            }
        }

        Ok(sessions)
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
