use sea_orm_migration::prelude::*;

/// How an episode's number was written (issue #334).
///
/// - `episodes.numbering`: `standard` (`S01E02`), `daily` (an air date) or
///   `absolute` (one running number across the series, `Show - 030`) -- the
///   `beam_domain::utils::media_path::EpisodeNumbering` the indexer read from
///   the file that created the row. Every existing row is `standard`: before
///   this an absolutely numbered file was indexed as season 1 (or its season
///   folder) episode N, indistinguishable from a standard one.
/// - `episodes.absolute_number`: the running number an absolutely numbered
///   file gave the episode, `NULL` for any other. Enrichment re-homes such an
///   episode onto the season and episode the provider's absolute order puts
///   it at, so this is how a rescan of the same file finds it there. Indexed
///   for that lookup.
///
/// `down()` drops both columns.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE episodes \
                 ADD COLUMN numbering TEXT NOT NULL DEFAULT 'standard' \
                     CONSTRAINT episodes_numbering \
                     CHECK (numbering IN ('standard', 'daily', 'absolute')), \
                 ADD COLUMN absolute_number INTEGER \
                     CONSTRAINT episodes_absolute_number CHECK (absolute_number > 0)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_episodes_absolute_number ON episodes (absolute_number) \
                 WHERE absolute_number IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE episodes DROP COLUMN absolute_number, DROP COLUMN numbering",
            )
            .await?;
        Ok(())
    }
}
