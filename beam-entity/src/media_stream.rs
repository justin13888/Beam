//! Media stream entity (renamed from stream to media_stream)

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "media_streams")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub file_id: Uuid,

    pub stream_index: i32,
    pub stream_type: StreamType,

    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,

    // Video-specific
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<f64>,
    pub bit_rate: Option<i64>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub hdr_format: Option<String>,

    // Audio-specific
    pub channels: Option<i32>,
    pub sample_rate: Option<i32>,
    pub channel_layout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "stream_type")]
pub enum StreamType {
    #[sea_orm(string_value = "video")]
    Video,
    #[sea_orm(string_value = "audio")]
    Audio,
    #[sea_orm(string_value = "subtitle")]
    Subtitle,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::files::Entity",
        from = "Column::FileId",
        to = "super::files::Column::Id"
    )]
    File,
}

impl Related<super::files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::File.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
