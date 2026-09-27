use sea_orm_migration::MigratorTraitSelf;
pub use sea_orm_migration::prelude::*;

mod m20260209_000001_create_schema;
mod m20260210_000001_create_users;

mod m20260212_000001_ensure_cascade;
mod m20260222_000001_create_admin_log;
mod m20260522_000001_add_file_mtime;
mod m20260704_000001_drop_stream_cache;
mod m20260704_000002_create_sessions;
mod m20260704_000003_metadata_enrichment;
mod m20260704_000004_add_enrichment_admin_log_category;
mod m20260704_000005_enable_pg_trgm;
mod m20260704_000006_playback_progress;
mod m20260705_000001_oidc_auth;
mod m20260706_000001_users_oidc_only;
mod m20260711_000001_users_email_optional;
mod m20260711_000002_users_disabled;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260209_000001_create_schema::Migration),
            Box::new(m20260210_000001_create_users::Migration),
            Box::new(m20260212_000001_ensure_cascade::Migration),
            Box::new(m20260222_000001_create_admin_log::Migration),
            Box::new(m20260522_000001_add_file_mtime::Migration),
            Box::new(m20260704_000001_drop_stream_cache::Migration),
            Box::new(m20260704_000002_create_sessions::Migration),
            Box::new(m20260704_000003_metadata_enrichment::Migration),
            Box::new(m20260704_000004_add_enrichment_admin_log_category::Migration),
            Box::new(m20260704_000005_enable_pg_trgm::Migration),
            Box::new(m20260704_000006_playback_progress::Migration),
            Box::new(m20260705_000001_oidc_auth::Migration),
            Box::new(m20260706_000001_users_oidc_only::Migration),
            Box::new(m20260711_000001_users_email_optional::Migration),
            Box::new(m20260711_000002_users_disabled::Migration),
        ]
    }
}

/// Apply `M`'s pending migrations (at most `steps` of them) as one unit: either
/// every one of them commits, or none does.
///
/// sea-orm-migration 2 wraps each migration in its own transaction on
/// Postgres, so a batch A, B in which B fails leaves A committed. An image
/// rolled back after that half-upgrade refuses to start, because its migrator
/// has no file for the recorded version A. Running `up` inside a transaction
/// Beam owns turns each of sea-orm's per-migration transactions into a
/// savepoint (`begin` on a `DatabaseTransaction` issues `SAVEPOINT`), so a
/// failure anywhere in the batch rolls the whole batch back and the database
/// stays at the version the previous image expects. This is what
/// sea-orm-migration 1.x did on Postgres by itself.
///
/// Every caller that applies migrations -- `beam-server` at startup, the
/// `beam-migration up` CLI, the `pg-integration` tier -- goes through this, so
/// there is one upgrade path.
pub async fn up_all_or_nothing<'c, M, C>(db: C, steps: Option<u32>) -> Result<(), DbErr>
where
    M: MigratorTrait,
    C: IntoSchemaManagerConnection<'c>,
{
    use sea_orm::TransactionTrait;

    let executor = db.into_database_executor();
    let batch = executor.begin().await?;
    match M::up(&batch, steps).await {
        Ok(()) => batch.commit().await,
        Err(migration_error) => match batch.rollback().await {
            Ok(()) => Err(migration_error),
            Err(rollback_error) => Err(DbErr::Custom(format!(
                "{migration_error}; rolling the migration batch back also failed: {rollback_error}"
            ))),
        },
    }
}

/// [`Migrator`] as the `beam-migration` CLI drives it: the same migrations,
/// except that `up` applies them through [`up_all_or_nothing`].
///
/// The CLI dispatches on [`MigratorTraitSelf`], whose blanket impl for every
/// [`MigratorTrait`] calls sea-orm's per-migration `up` directly, so the CLI
/// needs its own type to route `up` through the shared path. Every other
/// command falls through to the trait defaults, which read the same migration
/// list and table name.
pub struct CliMigrator;

#[async_trait::async_trait]
impl MigratorTraitSelf for CliMigrator {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        <Migrator as MigratorTrait>::migrations()
    }

    fn migration_table_name(&self) -> sea_orm::DynIden {
        <Migrator as MigratorTrait>::migration_table_name()
    }

    async fn up<'c, C>(&self, db: C, steps: Option<u32>) -> Result<(), DbErr>
    where
        C: IntoSchemaManagerConnection<'c>,
    {
        up_all_or_nothing::<Migrator, C>(db, steps).await
    }
}
