use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// OIDC `iss` claim. Unique together with `oidc_subject`, the
    /// JIT-provisioning lookup key (see ADR-0003).
    pub oidc_issuer: String,
    /// OIDC `sub` claim.
    pub oidc_subject: String,
    /// OIDC `email` claim, if the IdP released one. Not unique: the same
    /// email can legitimately appear under more than one issuer, and it
    /// drives admin-allowlist matching only, never identity.
    pub email: Option<String>,
    /// OIDC `name` claim (or `preferred_username` fallback), refreshed on
    /// every login.
    pub display_name: String,
    /// OIDC `picture` claim, refreshed on every login.
    pub avatar_url: Option<String>,
    /// Recomputed from the admin-email allowlist on every login; never
    /// trusted as durable state alone (see `docs/architecture/security.md`).
    pub is_admin: bool,
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
