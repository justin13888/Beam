//! What Beam made of one video file's trickplay sheets (issue #265).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "file_trickplay")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub file_id: Uuid,
    /// The content hash of the file the sheets were made from, its `u64`
    /// bits as `files.hash_xxh3` holds them.
    pub file_hash: i64,
    /// The layout columns, every one set or every one `NULL` (a `CHECK`
    /// holds them together): `NULL` when the file gave no sheets.
    pub interval_ms: Option<i32>,
    pub tile_width: Option<i32>,
    pub tile_height: Option<i32>,
    pub columns: Option<i32>,
    pub rows: Option<i32>,
    pub tile_count: Option<i32>,
    pub generated_at: DateTimeWithTimeZone,
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
