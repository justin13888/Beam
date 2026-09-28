//! A user's watched state for one movie or one episode (issue #188; see
//! docs/architecture/data-model.md).
//!
//! Exactly one of `movie_id` and `episode_id` is set, and `show_id` is set
//! exactly when `episode_id` is: the table's CHECK constraints hold both.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "watch_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Uuid,
    pub movie_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    /// The episode's show, denormalised so a show's rows are read without a
    /// join.
    pub show_id: Option<Uuid>,
    /// The file the last report named; set to NULL when that file is purged.
    pub last_file_id: Option<Uuid>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub play_count: i32,
    pub last_played_at: DateTimeWithTimeZone,
    pub dismissed_at: Option<DateTimeWithTimeZone>,
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
        belongs_to = "super::episode::Entity",
        from = "Column::EpisodeId",
        to = "super::episode::Column::Id"
    )]
    Episode,
    #[sea_orm(
        belongs_to = "super::show::Entity",
        from = "Column::ShowId",
        to = "super::show::Column::Id"
    )]
    Show,
    #[sea_orm(
        belongs_to = "super::files::Entity",
        from = "Column::LastFileId",
        to = "super::files::Column::Id"
    )]
    LastFile,
}

impl ActiveModelBehavior for ActiveModel {}
