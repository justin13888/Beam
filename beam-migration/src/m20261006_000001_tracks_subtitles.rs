use sea_orm_migration::prelude::*;

/// One track model with real codecs, and a primary source chosen at read
/// time (issue #189).
///
/// * `media_streams.codec` is rewritten in FFmpeg's own codec names (`h264`,
///   `hevc`, `eac3`, `subrip`, `hdmv_pgs_subtitle`), the vocabulary the
///   indexer now writes. The rows it wrote before held two other spellings:
///   the Rust `Debug` name of FFmpeg's codec id for video and audio (`H264`,
///   `EAC3`), which is FFmpeg's name upper-cased -- `beam-index` pins that for
///   every codec a library commonly holds -- and, for subtitles, the prober's
///   display names (`SubRip`, `ASS/SSA`, `WebVTT`), `Other("name")` around a
///   name it had no variant for, and `Unknown` for no codec at all. Each of
///   those gets its own arm; everything else is lower-cased.
/// * `media_streams.is_hearing_impaired` -- the stream's SDH/CC disposition.
///   `false` on existing rows until their file is probed again.
/// * `files.is_primary` and `movie_entries.is_primary` are dropped. The
///   indexer wrote `true` to every row and nothing read either; which source
///   plays by default is now ranked from the files themselves when read
///   (`beam_domain::utils::source_rank`), so there is nothing to store and
///   nothing to go stale.
///
/// `down()` puts both columns back -- every row `true`, as the indexer wrote
/// them, under their original `false` default -- and drops
/// `is_hearing_impaired`. It leaves the codec names as they are: the old
/// spellings cannot be recovered from the new ones, and the code before this
/// migration lower-cased a codec before reading it anyway.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "UPDATE media_streams SET codec = CASE \
                 WHEN codec = 'SubRip' THEN 'subrip' \
                 WHEN codec = 'ASS/SSA' THEN 'ass' \
                 WHEN codec = 'WebVTT' THEN 'webvtt' \
                 WHEN codec = 'Unknown' THEN 'none' \
                 WHEN codec LIKE 'Other(\"%\")' \
                     THEN substring(codec FROM 8 FOR char_length(codec) - 9) \
                 ELSE lower(codec) \
             END",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE media_streams \
             ADD COLUMN is_hearing_impaired BOOLEAN NOT NULL DEFAULT false",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE files DROP COLUMN is_primary")
            .await?;
        db.execute_unprepared("ALTER TABLE movie_entries DROP COLUMN is_primary")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        for table in ["movie_entries", "files"] {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ADD COLUMN is_primary BOOLEAN NOT NULL DEFAULT true"
            ))
            .await?;
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ALTER COLUMN is_primary SET DEFAULT false"
            ))
            .await?;
        }
        db.execute_unprepared("ALTER TABLE media_streams DROP COLUMN is_hearing_impaired")
            .await?;

        Ok(())
    }
}
