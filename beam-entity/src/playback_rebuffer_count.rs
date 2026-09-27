//! One UTC day's rebuffer events, with their total duration and duration
//! histogram, for one combination of coarse dimensions (issue #143).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "playback_rebuffer_counts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub day: Date,
    #[sea_orm(primary_key, auto_increment = false)]
    pub client_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub container: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub video_codec: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub height_class: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub bitrate_class: String,
    pub events: i64,
    pub total_ms: i64,
    /// Events shorter than one second.
    pub lt_1s: i64,
    /// Events of at least one and under three seconds.
    pub s1_3: i64,
    /// Events of at least three and under ten seconds.
    pub s3_10: i64,
    /// Events of at least ten and under thirty seconds.
    pub s10_30: i64,
    /// Events of thirty seconds or more.
    pub ge_30s: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
