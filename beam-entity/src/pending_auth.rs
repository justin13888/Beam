use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A single-use OIDC authorization round-trip record. Created when the login
/// redirect is issued (holding the `state`/`nonce`/PKCE verifier the
/// callback needs to complete the exchange), and consumed atomically -- a
/// `state` value can be exchanged for its row at most once -- when the
/// callback arrives.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pending_auths")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    /// Where to send the browser after a successful callback. Sanitized
    /// before storage (see the OIDC login handler) so this is always
    /// same-origin-relative, never an absolute or protocol-relative URL.
    pub redirect_path: Option<String>,
    pub created_at: DateTimeWithTimeZone,
    pub expires_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
