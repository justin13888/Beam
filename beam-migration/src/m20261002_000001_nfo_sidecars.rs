use sea_orm_migration::prelude::*;

/// What reading the metadata beside the media stores (issue #184).
///
/// - `movies.pinned_ref` and `shows.pinned_ref`: the provider id an NFO pins
///   the title to, as `"provider:id"` (`tmdb:603`). Enrichment fetches a
///   pinned title by that id rather than searching for it, and a file whose
///   NFO names the same id joins the same title. Unique, so one id pins one
///   title; nullable, and Postgres lets any number of `NULL`s share a unique
///   index.
/// - `sidecar_subtitles`: a text subtitle file beside a video, recorded as a
///   subtitle of that video (decision D184-1: its own table, not a
///   `media_streams` row, whose `stream_index` is the container's and whose
///   rows are replaced wholesale when the file is re-probed). A row goes with
///   its video file (`ON DELETE CASCADE`), and with its library. `path` is
///   unique so a scan upserts by it. `format` is one of the text formats
///   Beam indexes (decision D184-4); `language` is ISO 639-2/B.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        for table in ["movies", "shows"] {
            db.execute_unprepared(&format!("ALTER TABLE {table} ADD COLUMN pinned_ref TEXT"))
                .await?;
            db.execute_unprepared(&format!(
                "CREATE UNIQUE INDEX idx_{table}_pinned_ref ON {table} (pinned_ref)"
            ))
            .await?;
        }

        db.execute_unprepared(
            "CREATE TABLE sidecar_subtitles ( \
                 id UUID PRIMARY KEY, \
                 file_id UUID NOT NULL REFERENCES files (id) ON DELETE CASCADE, \
                 library_id UUID NOT NULL REFERENCES libraries (id) ON DELETE CASCADE, \
                 path TEXT NOT NULL, \
                 format TEXT NOT NULL \
                     CONSTRAINT sidecar_subtitles_format \
                     CHECK (format IN ('srt', 'vtt', 'ass', 'ssa')), \
                 language TEXT, \
                 title TEXT, \
                 is_forced BOOLEAN NOT NULL DEFAULT false, \
                 is_sdh BOOLEAN NOT NULL DEFAULT false, \
                 is_default BOOLEAN NOT NULL DEFAULT false, \
                 size_bytes BIGINT NOT NULL \
                     CONSTRAINT sidecar_subtitles_size CHECK (size_bytes >= 0), \
                 mtime TIMESTAMPTZ, \
                 created_at TIMESTAMPTZ NOT NULL, \
                 updated_at TIMESTAMPTZ NOT NULL, \
                 CONSTRAINT sidecar_subtitles_path_unique UNIQUE (path) \
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_sidecar_subtitles_file_id ON sidecar_subtitles (file_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_sidecar_subtitles_library_id ON sidecar_subtitles (library_id)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP TABLE sidecar_subtitles")
            .await?;
        // Dropping a column drops the index on it.
        for table in ["shows", "movies"] {
            db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN pinned_ref"))
                .await?;
        }

        Ok(())
    }
}
