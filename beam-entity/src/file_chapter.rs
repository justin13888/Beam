//! One chapter of a video file, as its container lists it (issue #265).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "file_chapters")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub file_id: Uuid,
    /// The chapter's position among its file's, from 0.
    #[sea_orm(primary_key, auto_increment = false)]
    pub chapter_index: i32,
    /// Milliseconds from the file's start; a `CHECK` holds it to at least 0.
    pub start_ms: i64,
    /// Milliseconds from the file's start; a `CHECK` holds it to at least
    /// `start_ms`.
    pub end_ms: i64,
    pub title: Option<String>,
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
}

impl Related<super::files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::File.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
