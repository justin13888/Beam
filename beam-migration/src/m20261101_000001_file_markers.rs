use sea_orm_migration::prelude::*;

/// The skip markers Beam keeps for each video file (issue #332).
///
/// - `file_markers`: at most one marker of each kind per file, keyed by the
///   file and the kind, set by an administrator (`admin`) or found by the
///   background detector (`detected`). Times are milliseconds from the
///   file's start, a marker always ending after it starts. Goes with its
///   file.
/// - `file_marker_detections`: one row per file the detector has listened
///   to, for the content hash (`files.hash_xxh3`'s bits) it heard, whether
///   or not it found a marker -- so a file is decoded again only once its
///   content changes. Goes with its file.
///
/// Nothing is backfilled: every file starts with no kept marker, and with
/// its chapter-named ones as before.
///
/// `down()` drops both tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE file_markers (\
                 file_id UUID NOT NULL REFERENCES files (id) ON DELETE CASCADE, \
                 kind TEXT NOT NULL \
                     CONSTRAINT file_markers_kind \
                     CHECK (kind IN ('recap', 'intro', 'credits', 'preview')), \
                 start_ms BIGINT NOT NULL, \
                 end_ms BIGINT NOT NULL, \
                 source TEXT NOT NULL \
                     CONSTRAINT file_markers_source CHECK (source IN ('admin', 'detected')), \
                 PRIMARY KEY (file_id, kind), \
                 CONSTRAINT file_markers_span CHECK (start_ms >= 0 AND end_ms > start_ms)\
             )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE file_marker_detections (\
                 file_id UUID PRIMARY KEY REFERENCES files (id) ON DELETE CASCADE, \
                 file_hash BIGINT NOT NULL, \
                 detected_at TIMESTAMPTZ NOT NULL\
             )",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE file_marker_detections")
            .await?;
        db.execute_unprepared("DROP TABLE file_markers").await?;
        Ok(())
    }
}
