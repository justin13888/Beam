use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TYPE admin_log_category ADD VALUE IF NOT EXISTS 'enrichment'")
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Postgres has no DROP VALUE for enum types; removing a value would
        // require rebuilding the type. Not reversible -- acceptable given
        // this project's pre-alpha "destructive migrations OK" stance.
        Ok(())
    }
}
