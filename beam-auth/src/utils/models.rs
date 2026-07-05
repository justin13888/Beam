use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Represents a user in the system.
#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Parameters for creating a new user.
#[derive(Debug, Clone)]
pub struct CreateUser {
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

impl From<beam_entity::user::Model> for User {
    fn from(model: beam_entity::user::Model) -> Self {
        Self {
            id: model.id,
            username: model.username,
            email: model.email,
            password_hash: model.password_hash,
            is_admin: model.is_admin,
            oidc_issuer: model.oidc_issuer,
            oidc_subject: model.oidc_subject,
            display_name: model.display_name,
            avatar_url: model.avatar_url,
            created_at: model.created_at.into(),
            updated_at: model.updated_at.into(),
        }
    }
}
