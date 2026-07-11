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
    /// email can legitimately appear under more than one issuer. Informational
    /// and for display only -- never identity, and no longer used for admin
    /// (admin is derived from a configured claim; see issue #85).
    pub email: Option<String>,
    /// OIDC `name` claim (or `preferred_username` fallback), refreshed on
    /// every login.
    pub display_name: String,
    /// OIDC `picture` claim, refreshed on every login.
    pub avatar_url: Option<String>,
    /// Recomputed from the IdP-asserted admin claim (`BEAM_OIDC_ADMIN_CLAIM`)
    /// on every login -- granting and revoking; never trusted as durable state
    /// alone (see `docs/architecture/security.md` and issue #85).
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
