use sea_orm_migration::prelude::*;

/// Adds `identity_key` to `movies` and `shows` -- what the indexer matches a
/// file to an existing title by, kept apart from the display `title` that
/// enrichment rewrites (issue #183).
///
/// Unique, so find-or-create is one `INSERT ... ON CONFLICT (identity_key) DO
/// NOTHING` and two files of one title indexed at once cannot create two
/// rows. Nullable, because the key of a row that predates this migration
/// cannot be computed in SQL: its `title` may already be the provider's, not
/// the filename parse the key is derived from. The indexer backfills those
/// rows from their files' paths; a `NULL` key is never matched, and Postgres
/// lets any number of `NULL`s share a unique index.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE movies ADD COLUMN identity_key TEXT")
            .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX idx_movies_identity_key ON movies (identity_key)",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE shows ADD COLUMN identity_key TEXT")
            .await?;
        db.execute_unprepared("CREATE UNIQUE INDEX idx_shows_identity_key ON shows (identity_key)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Dropping a column drops the index on it.
        db.execute_unprepared("ALTER TABLE shows DROP COLUMN identity_key")
            .await?;
        db.execute_unprepared("ALTER TABLE movies DROP COLUMN identity_key")
            .await?;

        Ok(())
    }
}
