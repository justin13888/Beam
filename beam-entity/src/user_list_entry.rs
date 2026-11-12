//! One movie or show on one of a user's lists (issue #262): the watchlist or
//! favourites.
//!
//! Exactly one of `movie_id` and `show_id` is set, and `list` is `watchlist`
//! or `favourites` (`beam_domain::models::UserList::as_str`): the table's
//! `CHECK`s hold both.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user_list_entries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Uuid,
    /// `watchlist` or `favourites`.
    pub list: String,
    pub movie_id: Option<Uuid>,
    pub show_id: Option<Uuid>,
    pub added_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::movie::Entity",
        from = "Column::MovieId",
        to = "super::movie::Column::Id"
    )]
    Movie,
    #[sea_orm(
        belongs_to = "super::show::Entity",
        from = "Column::ShowId",
        to = "super::show::Column::Id"
    )]
    Show,
}

impl ActiveModelBehavior for ActiveModel {}
