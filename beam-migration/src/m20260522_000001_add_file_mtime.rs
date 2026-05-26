use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Filesystem modification time, used together with file_size as the cheap
        // change-detection gate before an XXH3 rehash. Nullable: pre-existing rows
        // get NULL and are treated as "suspected changed" on the next scan.
        db.execute_unprepared("ALTER TABLE files ADD COLUMN mtime TIMESTAMPTZ")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files DROP COLUMN IF EXISTS mtime")
            .await?;

        Ok(())
    }
}
