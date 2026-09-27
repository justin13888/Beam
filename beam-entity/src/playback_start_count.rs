//! One UTC day's count of playback starts for one combination of coarse
//! dimensions (issue #143). No user, file or title: see
//! `m20260930_000001_playback_telemetry`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "playback_start_counts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub day: Date,
    #[sea_orm(primary_key, auto_increment = false)]
    pub client_kind: String,
    /// `started` or `failed`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub outcome: String,
    /// A failure reason; `none` exactly when `outcome` is `started`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub reason: String,
    /// A failure stage; `none` exactly when `outcome` is `started`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub stage: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub container: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub video_codec: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub audio_codec: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub height_class: String,
    pub count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
