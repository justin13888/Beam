//! Episode entity

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "episodes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub season_id: Uuid,

    pub episode_number: i32,
    pub title: String,
    pub description: Option<String>,
    pub air_date: Option<Date>,
    pub runtime_mins: Option<i32>,
    pub thumbnail_url: Option<String>,

    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::season::Entity",
        from = "Column::SeasonId",
        to = "super::season::Column::Id"
    )]
    Season,
    #[sea_orm(has_many = "super::files::Entity")]
    Files,
}

impl Related<super::season::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Season.def()
    }
}

impl Related<super::files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Files.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
