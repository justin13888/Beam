use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A user session, replacing what was previously stored in Redis.
///
/// `id` is the opaque session identifier the client presents (a random
/// URL-safe base64 string, matching the pre-Postgres scheme) -- not hashed
/// at rest in this commit. Hashing the token at rest is part of the OIDC
/// session-model redesign (see ADR-0005), not this storage-backend swap.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub user_id: Uuid,
    pub device_hash: String,
    pub ip: String,
    pub created_at: DateTimeWithTimeZone,
    pub last_active: DateTimeWithTimeZone,
    pub expires_at: DateTimeWithTimeZone,
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
