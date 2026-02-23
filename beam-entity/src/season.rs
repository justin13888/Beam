//! Season entity

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "seasons")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub show_id: Uuid,

    pub season_number: i32,
    pub poster_url: Option<String>,
    pub first_aired: Option<Date>,
    pub last_aired: Option<Date>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::show::Entity",
        from = "Column::ShowId",
        to = "super::show::Column::Id"
    )]
    Show,
    #[sea_orm(has_many = "super::episode::Entity")]
    Episodes,
}

impl Related<super::show::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Show.def()
    }
}

impl Related<super::episode::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Episodes.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
