use sea_orm_migration::prelude::*;

/// Adds `users.disabled` -- a local moderation switch, distinct from the
/// IdP-claim-driven `is_admin` (issue #85). Disabling an account revokes its
/// sessions and blocks any future login (enforced in `oidc_callback`); it is
/// beam's own durable state, never asserted by the IdP. Defaults to `FALSE`
/// so every existing account stays enabled after the migration.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE users ADD COLUMN disabled BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE users DROP COLUMN disabled")
            .await?;

        Ok(())
    }
}
