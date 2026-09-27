use sea_orm_migration::prelude::*;

/// What the path-inference rules of issue #182 need stored.
///
/// - `files.last_episode_number`: the end of a multi-episode file's range
///   (`S01E01E02`). The file is attached to the first episode; the range lives
///   on the file rather than as extra rows. A `CHECK` keeps it off anything
///   that is not an episode file.
/// - `files.classifier_version`: which version of the rules classified the
///   row. Existing rows get `0`, so the first scan after the upgrade
///   reclassifies every one of them from its path.
/// - One movie entry per `(library, movie, edition)`. The old unique index let
///   any number of entries with a `NULL` edition coexist -- Postgres treats
///   `NULL`s as distinct -- and the indexer created one per file, so every
///   copy of a film was its own entry. Existing duplicates are merged into the
///   oldest (their files repointed), and the index is recreated `NULLS NOT
///   DISTINCT` so find-or-create can be one `ON CONFLICT` statement.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files ADD COLUMN last_episode_number INTEGER")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE files ADD CONSTRAINT files_last_episode_requires_episode \
             CHECK (last_episode_number IS NULL OR episode_id IS NOT NULL)",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE files ADD COLUMN classifier_version SMALLINT NOT NULL DEFAULT 0",
        )
        .await?;

        // A window partition groups `NULL` editions together, unlike the old
        // index: every duplicate maps to the oldest entry of its group.
        let duplicates = "SELECT id, first_value(id) OVER ( \
                              PARTITION BY library_id, movie_id, edition \
                              ORDER BY created_at, id) AS keep \
                            FROM movie_entries";
        db.execute_unprepared(&format!(
            "UPDATE files f SET movie_entry_id = d.keep \
               FROM ({duplicates}) d \
              WHERE f.movie_entry_id = d.id AND d.id <> d.keep"
        ))
        .await?;
        db.execute_unprepared(&format!(
            "DELETE FROM movie_entries me \
              USING ({duplicates}) d \
              WHERE me.id = d.id AND d.id <> d.keep"
        ))
        .await?;

        db.execute_unprepared("DROP INDEX idx_movie_entries_unique")
            .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX idx_movie_entries_unique \
               ON movie_entries (library_id, movie_id, edition) NULLS NOT DISTINCT",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Merged entries stay merged: which file belonged to which duplicate
        // was never meaningful.
        db.execute_unprepared("DROP INDEX idx_movie_entries_unique")
            .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX idx_movie_entries_unique \
               ON movie_entries (library_id, movie_id, edition)",
        )
        .await?;
        // Dropping a column drops the `CHECK` that reads it.
        db.execute_unprepared("ALTER TABLE files DROP COLUMN classifier_version")
            .await?;
        db.execute_unprepared("ALTER TABLE files DROP COLUMN last_episode_number")
            .await?;

        Ok(())
    }
}
