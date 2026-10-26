use sea_orm_migration::prelude::*;

/// Chapters and trickplay records of video files (issue #265).
///
/// - `file_chapters`: each file's chapters as its container lists them,
///   keyed by the file and the chapter's position. Times are milliseconds
///   from the file's start, a chapter never ending before it starts. Written
///   whole each time the file is probed; nothing is backfilled -- a file
///   indexed before this has none until its next probe. Goes with its file.
/// - `file_trickplay`: one row per file whose trickplay sheets Beam tried to
///   make, for the content hash (`files.hash_xxh3`'s bits) it made them
///   from. The six layout columns are all set, or all `NULL` for a file that
///   gave no sheets. Goes with its file.
///
/// `down()` drops both tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE file_chapters (\
                 file_id UUID NOT NULL REFERENCES files (id) ON DELETE CASCADE, \
                 chapter_index INTEGER NOT NULL \
                     CONSTRAINT file_chapters_chapter_index CHECK (chapter_index >= 0), \
                 start_ms BIGINT NOT NULL, \
                 end_ms BIGINT NOT NULL, \
                 title TEXT, \
                 PRIMARY KEY (file_id, chapter_index), \
                 CONSTRAINT file_chapters_span CHECK (start_ms >= 0 AND end_ms >= start_ms)\
             )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE file_trickplay (\
                 file_id UUID PRIMARY KEY REFERENCES files (id) ON DELETE CASCADE, \
                 file_hash BIGINT NOT NULL, \
                 interval_ms INTEGER, \
                 tile_width INTEGER, \
                 tile_height INTEGER, \
                 columns INTEGER, \
                 rows INTEGER, \
                 tile_count INTEGER, \
                 generated_at TIMESTAMPTZ NOT NULL, \
                 CONSTRAINT file_trickplay_layout CHECK (\
                     (interval_ms IS NULL AND tile_width IS NULL AND tile_height IS NULL \
                         AND columns IS NULL AND rows IS NULL AND tile_count IS NULL) \
                     OR (interval_ms > 0 AND tile_width > 0 AND tile_height > 0 \
                         AND columns > 0 AND rows > 0 AND tile_count > 0))\
             )",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE file_trickplay").await?;
        db.execute_unprepared("DROP TABLE file_chapters").await?;
        Ok(())
    }
}
