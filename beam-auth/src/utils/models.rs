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
    /// Local moderation switch, distinct from the IdP-driven `is_admin`:
    /// disabling revokes the account's sessions and blocks any future login
    /// (issue #85). Never asserted by the IdP; managed only through the admin
    /// API. JIT-provisioned users are always created enabled.
    pub disabled: bool,
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
            disabled: model.disabled,
            created_at: model.created_at.into(),
            updated_at: model.updated_at.into(),
        }
    }
}

/// Builders for tests, beside the type they build.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::CreateUser;

    /// A first-login provisioning request for `subject` at `issuer`: an
    /// ordinary, non-admin user with a name and nothing else. Override a
    /// field with struct-update syntax where a test needs another.
    #[must_use]
    pub fn create_user(issuer: &str, subject: &str) -> CreateUser {
        CreateUser {
            oidc_issuer: issuer.to_owned(),
            oidc_subject: subject.to_owned(),
            email: None,
            display_name: "Ada".to_owned(),
            avatar_url: None,
            is_admin: false,
        }
    }
}
