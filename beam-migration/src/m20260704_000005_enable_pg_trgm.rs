use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS pg_trgm")
            .await?;

        db.execute_unprepared(
            "CREATE INDEX idx_movies_title_trgm ON movies USING GIN (title gin_trgm_ops)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_shows_title_trgm ON shows USING GIN (title gin_trgm_ops)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX IF EXISTS idx_shows_title_trgm")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_movies_title_trgm")
            .await?;
        // The pg_trgm extension is left installed on down -- other objects
        // (or a future migration) may depend on it, and dropping shared
        // extensions on a per-migration rollback is riskier than leaving an
        // unused extension installed.

        Ok(())
    }
}
