//! One UTC day's count of source switches for one combination of coarse
//! dimensions (issue #143).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "playback_switch_counts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub day: Date,
    #[sea_orm(primary_key, auto_increment = false)]
    pub client_kind: String,
    /// `manual` or `auto`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub trigger: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub from_height_class: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub to_height_class: String,
    pub count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
