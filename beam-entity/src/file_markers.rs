//! The skip markers Beam keeps for a video file (issue #332): one per kind,
//! set by an administrator or found by the background detector.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "file_markers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub file_id: Uuid,
    /// `recap`, `intro`, `credits` or `preview`; a `CHECK` holds it to those.
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: String,
    /// Milliseconds from the file's start; a `CHECK` holds it to at least 0.
    pub start_ms: i64,
    /// Milliseconds from the file's start; a `CHECK` holds it after
    /// `start_ms`.
    pub end_ms: i64,
    /// `admin` or `detected`; a `CHECK` holds it to those.
    pub source: String,
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

/// The version of each video file the marker detector last listened to
/// (issue #332).
pub mod detection {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "file_marker_detections")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub file_id: Uuid,
        /// The content hash of the file the detector heard, its `u64` bits as
        /// `files.hash_xxh3` holds them.
        pub file_hash: i64,
        pub detected_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "crate::files::Entity",
            from = "Column::FileId",
            to = "crate::files::Column::Id",
            on_delete = "Cascade"
        )]
        File,
    }

    impl Related<crate::files::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::File.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}
