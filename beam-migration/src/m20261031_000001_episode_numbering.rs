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
/// - `episodes.absolute_season_number`: the season the absolutely numbered
///   file was indexed into -- season 1, or its season folder's -- set exactly
///   when `absolute_number` is. A running number is only unique within the
///   season it was read in: `Season 1/Show - 03` and `Season 2/Show - 03`
///   are two episodes, so a rescan looks an episode up by the pair.
///
/// Existing rows are not backfilled -- nothing records how their file was
/// numbered. An absolutely numbered file that a rescan finds on a
/// `standard` row with no running number upgrades that row instead (see
/// `ShowRepository::find_or_create_episode`).
///
/// `down()` drops all three columns.
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
                     CONSTRAINT episodes_absolute_number CHECK (absolute_number > 0), \
                 ADD COLUMN absolute_season_number INTEGER \
                     CONSTRAINT episodes_absolute_season_number \
                     CHECK (absolute_season_number >= 0), \
                 ADD CONSTRAINT episodes_absolute_place \
                     CHECK ((absolute_number IS NULL) = (absolute_season_number IS NULL))",
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
                "ALTER TABLE episodes DROP COLUMN absolute_season_number, \
                 DROP COLUMN absolute_number, DROP COLUMN numbering",
            )
            .await?;
        Ok(())
    }
}
