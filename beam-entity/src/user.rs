use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub username: String,
    #[sea_orm(unique)]
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    /// OIDC `iss` claim. Present only for OIDC-provisioned users; `NULL` for
    /// password-only accounts until the auth cutover (ADR-0003).
    pub oidc_issuer: Option<String>,
    /// OIDC `sub` claim. Unique together with `oidc_issuer`, the
    /// JIT-provisioning lookup key.
    pub oidc_subject: Option<String>,
    /// OIDC `name` claim (or `preferred_username` fallback), refreshed on
    /// every login.
    pub display_name: Option<String>,
    /// OIDC `picture` claim, refreshed on every login.
    pub avatar_url: Option<String>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::session::Entity")]
    Sessions,
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
