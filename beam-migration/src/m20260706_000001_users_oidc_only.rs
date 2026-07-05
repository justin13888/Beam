use sea_orm_migration::prelude::*;

/// Destructive `users` rework completing the auth cutover (ADR-0003):
/// password auth is gone, so `username`/`password_hash` are dropped and
/// `oidc_issuer`/`oidc_subject`/`display_name` become required. Pre-alpha
/// software, no real user data to preserve -- see `docs/architecture/
/// data-model.md`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Any account never linked to an OIDC identity was a password-only
        // account -- there is no way to log into it anymore, so it can't be
        // migrated forward. Backfill display_name from username for the
        // survivors before username disappears.
        db.execute_unprepared(
            "DELETE FROM users WHERE oidc_issuer IS NULL OR oidc_subject IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE users SET display_name = username WHERE display_name IS NULL",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE users ALTER COLUMN oidc_issuer SET NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN oidc_subject SET NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN display_name SET NOT NULL")
            .await?;

        db.execute_unprepared("ALTER TABLE users DROP COLUMN username")
            .await?;
        db.execute_unprepared("ALTER TABLE users DROP COLUMN password_hash")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE users ADD COLUMN username TEXT NOT NULL DEFAULT ''")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE users ADD COLUMN password_hash TEXT NOT NULL DEFAULT ''",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN oidc_issuer DROP NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN oidc_subject DROP NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN display_name DROP NOT NULL")
            .await?;

        Ok(())
    }
}
