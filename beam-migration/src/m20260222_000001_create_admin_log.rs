use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TYPE admin_log_level AS ENUM ('info', 'warning', 'error')",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TYPE admin_log_category AS ENUM ('library_scan', 'system', 'auth')",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE admin_logs (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                level admin_log_level NOT NULL,
                category admin_log_category NOT NULL,
                message TEXT NOT NULL,
                details JSONB,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE INDEX idx_admin_logs_created_at ON admin_logs (created_at DESC)",
        )
        .await?;

        db.execute_unprepared(
            "CREATE INDEX idx_admin_logs_level ON admin_logs (level)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP TABLE IF EXISTS admin_logs").await?;
        db.execute_unprepared("DROP TYPE IF EXISTS admin_log_category").await?;
        db.execute_unprepared("DROP TYPE IF EXISTS admin_log_level").await?;

        Ok(())
    }
}
