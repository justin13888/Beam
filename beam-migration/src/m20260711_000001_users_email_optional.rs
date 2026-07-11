use sea_orm_migration::prelude::*;

/// Relaxes `users.email` from `NOT NULL UNIQUE` to nullable and non-unique
/// (issue #79). The `NOT NULL UNIQUE` was a leftover of the password-auth era
/// (`m20260210_000001_create_users`); after the OIDC cutover (ADR-0003) the
/// entity and JIT-provisioning path treat email as optional and non-unique
/// per `docs/architecture/data-model.md`: the same email may legitimately
/// appear under more than one issuer, and it is informational only, never
/// identity (admin is derived from a configured ID-token claim, not email;
/// see issue #85). An IdP that releases no `email` claim must not break
/// JIT provisioning, but the leftover constraint would reject the `NULL`
/// insert at the database. Drops the constraint auto-named `users_email_key`
/// by sea-query's `.unique_key()`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE users ALTER COLUMN email DROP NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users DROP CONSTRAINT IF EXISTS users_email_key")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Best-effort restore of the pre-#79 shape. This can fail if the data
        // now contains NULL or duplicate emails (exactly what the up migration
        // makes legal), so it is only meaningful on an otherwise-empty table.
        db.execute_unprepared("ALTER TABLE users ALTER COLUMN email SET NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE users ADD CONSTRAINT users_email_key UNIQUE (email)")
            .await?;

        Ok(())
    }
}
