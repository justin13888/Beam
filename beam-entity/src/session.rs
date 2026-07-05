use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A user session, shared by the legacy password-JWT-refresh flow and the
/// OIDC BFF cookie flow (see ADR-0003/ADR-0005).
///
/// `id` is a stable internal identifier used only for listing/revoking a
/// session without ever re-exposing its credential. The credential itself
/// (an opaque, random, URL-safe base64 token handed to the client as a
/// cookie value or refresh-session id) is never stored -- only its SHA-256
/// hex digest, in `token_hash`. Expiry is two-tiered: `idle_expires_at`
/// slides forward on activity (via `touch`) up to the hard
/// `absolute_expires_at` ceiling, which `touch` never extends past.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Uuid,
    #[sea_orm(unique)]
    pub token_hash: String,
    pub device_hash: String,
    pub ip: String,
    pub created_at: DateTimeWithTimeZone,
    pub last_active: DateTimeWithTimeZone,
    pub idle_expires_at: DateTimeWithTimeZone,
    pub absolute_expires_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
