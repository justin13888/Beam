use sea_orm_migration::prelude::*;

/// Adds `files.missing_since` -- the soft-delete stamp for a file the indexer
/// can no longer find on disk (issue #179).
///
/// `NULL` means the file is present. A timestamp means a scan or the watcher
/// first found the path gone at that instant: the row is hidden from every
/// browse, detail, search and stream read, but it keeps its id -- and with it
/// every `playback_progress` row that references it -- until the path either
/// reappears (the stamp is cleared) or stays gone for the configured grace
/// period, when a healthy scan purges the row. Nullable with no default, so
/// every existing row reads as present after the migration.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files ADD COLUMN missing_since TIMESTAMPTZ")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files DROP COLUMN missing_since")
            .await?;

        Ok(())
    }
}
