use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Represents a user in the system. Identity is `(oidc_issuer, oidc_subject)`
/// -- there is no password (see ADR-0003).
#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub oidc_issuer: String,
    pub oidc_subject: String,
    pub email: Option<String>,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Parameters for JIT-provisioning a new user on first OIDC login.
#[derive(Debug, Clone)]
pub struct CreateUser {
    pub oidc_issuer: String,
    pub oidc_subject: String,
    pub email: Option<String>,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
}

impl From<beam_entity::user::Model> for User {
    fn from(model: beam_entity::user::Model) -> Self {
        Self {
            id: model.id,
            oidc_issuer: model.oidc_issuer,
            oidc_subject: model.oidc_subject,
            email: model.email,
            display_name: model.display_name,
            avatar_url: model.avatar_url,
            is_admin: model.is_admin,
            created_at: model.created_at.into(),
            updated_at: model.updated_at.into(),
        }
    }
}
