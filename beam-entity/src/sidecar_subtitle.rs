//! Sidecar subtitle entity: a text subtitle file beside a video (issue #184).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sidecar_subtitles")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The video file the subtitle belongs to.
    pub file_id: Uuid,
    pub library_id: Uuid,
    #[sea_orm(unique)]
    pub path: String,
    /// `srt`, `vtt`, `ass` or `ssa` (a `CHECK` holds it to these).
    pub format: String,
    /// ISO 639-2/B.
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_forced: bool,
    pub is_sdh: bool,
    pub is_default: bool,
    pub size_bytes: i64,
    pub mtime: Option<DateTimeWithTimeZone>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::files::Entity",
        from = "Column::FileId",
        to = "super::files::Column::Id",
        on_delete = "Cascade"
    )]
    File,
    #[sea_orm(
        belongs_to = "super::library::Entity",
        from = "Column::LibraryId",
        to = "super::library::Column::Id",
        on_delete = "Cascade"
    )]
    Library,
}

impl Related<super::files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::File.def()
    }
}

impl Related<super::library::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Library.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
