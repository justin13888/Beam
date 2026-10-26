//! What an administrator restricted one user to (issue #263). A user with no
//! row is unrestricted.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_access_policies")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: Uuid,
    /// Every library, or only those `user_library_access` grants.
    pub all_libraries: bool,
    /// The highest age band the user may see
    /// (`beam_domain::models::access::ContentRating::as_str`, which a `CHECK`
    /// holds it to); `NULL` for no ceiling.
    pub max_content_rating: Option<String>,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
