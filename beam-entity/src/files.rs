//! File entity (renamed from indexed_file)

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "files")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    // Polymorphic: exactly ONE must be set
    pub movie_entry_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,

    pub library_id: Uuid,

    pub file_path: String,
    pub file_size: i64,
    pub mime_type: Option<String>,

    #[sea_orm(column_type = "BigInteger")]
    pub hash_xxh3: i64,

    pub duration_secs: Option<f64>,
    pub container_format: Option<String>,

    pub language: Option<String>,
    pub quality: Option<String>,
    pub release_group: Option<String>,

    pub is_primary: bool,

    pub scanned_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    pub file_status: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::library::Entity",
        from = "Column::LibraryId",
        to = "super::library::Column::Id"
    )]
    Library,
    #[sea_orm(
        belongs_to = "super::movie_entry::Entity",
        from = "Column::MovieEntryId",
        to = "super::movie_entry::Column::Id"
    )]
    MovieEntry,
    #[sea_orm(
        belongs_to = "super::episode::Entity",
        from = "Column::EpisodeId",
        to = "super::episode::Column::Id"
    )]
    Episode,
    #[sea_orm(has_many = "super::media_stream::Entity")]
    MediaStreams,
    #[sea_orm(has_many = "super::stream_cache::Entity")]
    StreamCaches,
}

impl Related<super::library::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Library.def()
    }
}

impl Related<super::movie_entry::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MovieEntry.def()
    }
}

impl Related<super::episode::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Episode.def()
    }
}

impl Related<super::media_stream::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MediaStreams.def()
    }
}

impl Related<super::stream_cache::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StreamCaches.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
