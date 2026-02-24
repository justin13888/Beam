use crate::utils::models::{CreateUser, User};
use crate::utils::repository::UserRepository;
use crate::utils::session_store::{SessionData, SessionStore};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use async_trait::async_trait;
use chrono::Utc;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("User already exists")]
    UserAlreadyExists,
    #[error("Database error: {0}")]
    Database(String),
    #[error("Session error: {0}")]
    Session(String),
    #[error("Token error: {0}")]
    Token(#[from] jsonwebtoken::errors::Error),
    #[error("Password hashing error: {0}")]
    PasswordHash(#[from] argon2::password_hash::Error),
}

type Result<T> = std::result::Result<T, AuthError>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // user_id
    pub sid: String, // session_id
    pub exp: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StreamClaims {
    pub sub: String, // user_id
    pub stream_id: String,
    pub exp: usize,
}

#[derive(Debug, Serialize)]
pub struct AuthUserResponse {
    /// User ID of authenticated user
    pub id: String,
    pub username: String,
    pub email: String,
    pub is_admin: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub session_id: String,
    pub user: AuthUserResponse,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: String,
    pub session_id: String,
}

#[async_trait]
pub trait AuthService: Send + Sync + std::fmt::Debug + 'static {
    /// Register a new user with the given details.
    async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
        device_hash: &str,
        ip: &str,
    ) -> Result<AuthResponse>;

    /// Login a user with username/email and password.
    async fn login(
        &self,
        username_or_email: &str,
        password: &str,
        device_hash: &str,
        ip: &str,
    ) -> Result<AuthResponse>;

    /// Refresh an existing session.
    async fn refresh(&self, session_id: &str) -> Result<AuthResponse>;

    /// Verify a JWT token and return the authenticated user.
    async fn verify_token(&self, token: &str) -> Result<AuthenticatedUser>;

    /// Logout a user by invalidating their session.
    async fn logout(&self, session_id: &str) -> Result<()>;

    /// Logout all sessions for a specific user.
    async fn logout_all(&self, user_id: &str) -> Result<u64>;

    /// Get all active sessions for a user.
    async fn get_sessions(&self, user_id: &str) -> Result<Vec<(String, SessionData)>>;

    /// Create a temporary token for accessing a specific stream.
    fn create_stream_token(&self, user_id: &str, stream_id: &str) -> Result<String>;

    /// Verify a stream token and return the associated stream ID.
    fn verify_stream_token(&self, token: &str) -> Result<String>;
}

#[derive(Debug)]
pub struct LocalAuthService {
    user_repo: Arc<dyn UserRepository>,
    session_store: Arc<dyn SessionStore>,
    jwt_secret: String,
}

impl LocalAuthService {
    pub fn new(
        user_repo: Arc<dyn UserRepository>,
        session_store: Arc<dyn SessionStore>,
        jwt_secret: String,
    ) -> Self {
        Self {
            user_repo,
            session_store,
            jwt_secret,
        }
    }

    fn hash_password(&self, password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)?
            .to_string();
        Ok(password_hash)
    }

    fn verify_password(&self, password: &str, password_hash: &str) -> bool {
        let parsed_hash = match PasswordHash::new(password_hash) {
            Ok(hash) => hash,
            Err(_) => return false,
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    }

    fn create_token(&self, user_id: &str, session_id: &str) -> Result<String> {
        let expiration = Utc::now()
            .checked_add_signed(chrono::Duration::minutes(15))
            .expect("valid timestamp")
            .timestamp() as usize;

        let claims = Claims {
            sub: user_id.to_string(),
            sid: session_id.to_string(),
            exp: expiration,
        };

        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )?;

        Ok(token)
    }

    async fn create_session(
        &self,
        user: &User,
        device_hash: &str,
        ip: &str,
    ) -> Result<(String, String)> {
        // 7 days session TTL
        let ttl_secs = 7 * 24 * 60 * 60;

        let session_data = SessionData {
            user_id: user.id.to_string(),
            device_hash: device_hash.to_string(),
            ip: ip.to_string(),
            created_at: Utc::now().timestamp(),
            last_active: Utc::now().timestamp(),
        };

        let session_id = self
            .session_store
            .create(&session_data, ttl_secs)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))?;

        let token = self.create_token(&user.id.to_string(), &session_id)?;

        Ok((token, session_id))
    }
}

#[async_trait]
impl AuthService for LocalAuthService {
    async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
        device_hash: &str,
        ip: &str,
    ) -> Result<AuthResponse> {
        if self
            .user_repo
            .find_by_username(username)
            .await
            .map_err(|e| AuthError::Database(e.to_string()))?
            .is_some()
        {
            return Err(AuthError::UserAlreadyExists);
        }

        if self
            .user_repo
            .find_by_email(email)
            .await
            .map_err(|e| AuthError::Database(e.to_string()))?
            .is_some()
        {
            return Err(AuthError::UserAlreadyExists);
        }

        let password_hash = self.hash_password(password)?;

        let user = CreateUser {
            username: username.to_string(),
            email: email.to_string(),
            password_hash,
            is_admin: false,
        };

        let user = self
            .user_repo
            .create(user)
            .await
            .map_err(|e| AuthError::Database(e.to_string()))?;

        let (access_token, session_id) = self.create_session(&user, device_hash, ip).await?;

        Ok(AuthResponse {
            token: access_token,
            session_id,
            user: AuthUserResponse {
                id: user.id.to_string(),
                username: user.username.clone(),
                email: user.email.clone(),
                is_admin: user.is_admin,
            },
        })
    }

    async fn login(
        &self,
        username_or_email: &str,
        password: &str,
        device_hash: &str,
        ip: &str,
    ) -> Result<AuthResponse> {
        // Try to find by username first, then email
        let user = if let Some(u) = self
            .user_repo
            .find_by_username(username_or_email)
            .await
            .map_err(|e| AuthError::Database(e.to_string()))?
        {
            Some(u)
        } else {
            self.user_repo
                .find_by_email(username_or_email)
                .await
                .map_err(|e| AuthError::Database(e.to_string()))?
        };

        let user = user.ok_or(AuthError::InvalidCredentials)?;

        if !self.verify_password(password, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        let (access_token, session_id) = self.create_session(&user, device_hash, ip).await?;

        Ok(AuthResponse {
            token: access_token,
            session_id,
            user: AuthUserResponse {
                id: user.id.to_string(),
                username: user.username,
                email: user.email,
                is_admin: user.is_admin,
            },
        })
    }

    async fn refresh(&self, session_id: &str) -> Result<AuthResponse> {
        let session = self
            .session_store
            .get(session_id)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))?
            .ok_or(AuthError::InvalidCredentials)?;

        // Touch session
        let ttl_secs = 7 * 24 * 60 * 60;
        self.session_store
            .touch(session_id, ttl_secs)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))?;

        // Create new token
        let access_token = self.create_token(&session.user_id, session_id)?;

        // Fetch username for response
        let user_uuid = Uuid::parse_str(&session.user_id).unwrap_or_default();
        let user = self
            .user_repo
            .find_by_id(user_uuid)
            .await
            .map_err(|e| AuthError::Database(e.to_string()))?
            .ok_or(AuthError::InvalidCredentials)?;

        Ok(AuthResponse {
            token: access_token,
            session_id: session_id.to_string(),
            user: AuthUserResponse {
                id: session.user_id,
                username: user.username,
                email: user.email,
                is_admin: user.is_admin,
            },
        })
    }

    async fn verify_token(&self, token: &str) -> Result<AuthenticatedUser> {
        let validation = Validation::default();
        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &validation,
        )?;

        // Verify session still exists
        let session_id = token_data.claims.sid;
        let session = self
            .session_store
            .get(&session_id)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))?;

        if session.is_none() {
            return Err(AuthError::InvalidCredentials); // Session revoked/expired
        }

        Ok(AuthenticatedUser {
            user_id: token_data.claims.sub,
            session_id,
        })
    }

    async fn logout(&self, session_id: &str) -> Result<()> {
        self.session_store
            .delete(session_id)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))
    }

    async fn logout_all(&self, user_id: &str) -> Result<u64> {
        self.session_store
            .delete_all_for_user(user_id)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))
    }

    async fn get_sessions(&self, user_id: &str) -> Result<Vec<(String, SessionData)>> {
        self.session_store
            .list_for_user(user_id)
            .await
            .map_err(|e| AuthError::Session(e.to_string()))
    }

    fn create_stream_token(&self, user_id: &str, stream_id: &str) -> Result<String> {
        let expiration = Utc::now()
            .checked_add_signed(chrono::Duration::hours(6)) // 6 hours validity
            .expect("valid timestamp")
            .timestamp() as usize;

        let claims = StreamClaims {
            sub: user_id.to_string(),
            stream_id: stream_id.to_string(),
            exp: expiration,
        };

        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )?;

        Ok(token)
    }

    fn verify_stream_token(&self, token: &str) -> Result<String> {
        let validation = Validation::default();
        let token_data = decode::<StreamClaims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &validation,
        )?;

        Ok(token_data.claims.stream_id)
    }
}
