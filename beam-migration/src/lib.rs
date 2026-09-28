use std::marker::PhantomData;

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
mod m20260927_000001_files_missing_since;
mod m20260928_000001_title_identity_key;
mod m20260928_000010_device_auths;
mod m20260929_000001_classifier_v2;
mod m20260930_000001_playback_telemetry;
mod m20261001_000001_files_unique_path;
mod m20261002_000001_nfo_sidecars;
mod m20261003_000001_catalogue_browse;
mod m20261007_000001_files_change_identity;
mod m20261008_000001_tracks_subtitles;
mod m20261009_000001_enrichment_locks;
mod m20261012_000001_movie_parts;

pub use m20261008_000001_tracks_subtitles::RENAMED_FFMPEG_CODECS;

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
            Box::new(m20260927_000001_files_missing_since::Migration),
            Box::new(m20260928_000001_title_identity_key::Migration),
            Box::new(m20260928_000010_device_auths::Migration),
            Box::new(m20260929_000001_classifier_v2::Migration),
            Box::new(m20260930_000001_playback_telemetry::Migration),
            Box::new(m20261001_000001_files_unique_path::Migration),
            Box::new(m20261002_000001_nfo_sidecars::Migration),
            Box::new(m20261003_000001_catalogue_browse::Migration),
            Box::new(m20261007_000001_files_change_identity::Migration),
            Box::new(m20261008_000001_tracks_subtitles::Migration),
            Box::new(m20261009_000001_enrichment_locks::Migration),
            Box::new(m20261012_000001_movie_parts::Migration),
        ]
    }
}

/// The Postgres advisory-lock key every [`up_all_or_nothing`] batch holds
/// while it runs.
///
/// The eight ASCII bytes `beam-mig` read as a big-endian `i64`. The value is
/// arbitrary but must never change: two releases that disagree on it would not
/// exclude each other during a rolling upgrade. It shares Postgres's single
/// advisory-lock keyspace with anything else using that database, so an
/// operator co-hosting Beam with another application that takes advisory locks
/// should know it.
pub const MIGRATION_LOCK_KEY: i64 = i64::from_be_bytes(*b"beam-mig");

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
/// On Postgres the batch is also serialised against every other migrator on
/// the same database: before reading the ledger it takes the transaction-scoped
/// advisory lock [`MIGRATION_LOCK_KEY`]. Two server processes starting at once
/// -- a rolling restart that briefly overlaps, a Kubernetes pod replaced while
/// its predecessor is still terminating, an operator running `beam-migration
/// up` beside a live server -- would otherwise both read an empty ledger and
/// both run the same `CREATE TABLE`, and the loser would exit with a duplicate
/// object error. With the lock the second waits, then reads the ledger the
/// first committed and finds nothing pending. The lock is released by the
/// commit or rollback that ends the batch, so a migrator that dies mid-batch
/// cannot leave it held.
///
/// Nothing on the way to the lock touches the database's schema: sea-orm
/// creates its ledger table (`CREATE TABLE IF NOT EXISTS seaql_migrations`)
/// inside the batch, after the lock. That `IF NOT EXISTS` is not safe against
/// a concurrent creator -- a second session creating the same table blocks on
/// the first's uncommitted catalog rows and then fails with a unique violation
/// on `pg_type` -- so any ledger DDL issued before the lock would reopen the
/// race on a fresh database. Read the ledger with sea-orm's `*_read_only`
/// methods anywhere outside a batch.
///
/// Every caller that applies migrations -- `beam-server` at startup (through
/// [`apply_pending`]), the `beam-migration up` CLI, the `pg-integration` tier
/// -- goes through this, so there is one upgrade path.
pub async fn up_all_or_nothing<'c, M, C>(db: C, steps: Option<u32>) -> Result<(), DbErr>
where
    M: MigratorTrait,
    C: IntoSchemaManagerConnection<'c>,
{
    locked_batch::<M, C>(db, steps).await.map(|_applied| ())
}

/// What `beam-server` runs at startup: every pending migration, through
/// [`up_all_or_nothing`]'s locked, all-or-nothing batch. Returns how many
/// migrations the batch applied.
///
/// The count is read inside the batch, under the lock, so it is exact: a
/// server that waited on another migrator reports 0, not what was pending
/// before it waited.
pub async fn apply_pending<M>(db: &sea_orm::DatabaseConnection) -> Result<usize, DbErr>
where
    M: MigratorTrait,
{
    locked_batch::<M, _>(db, None).await
}

/// The body of [`up_all_or_nothing`], returning how many migrations were
/// pending under the lock -- with `steps` of `None`, how many it applied.
async fn locked_batch<'c, M, C>(db: C, steps: Option<u32>) -> Result<usize, DbErr>
where
    M: MigratorTrait,
    C: IntoSchemaManagerConnection<'c>,
{
    use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

    let executor = db.into_database_executor();
    let batch = executor.begin().await?;
    // Postgres only: MySQL and SQLite have no `pg_advisory_xact_lock`, and
    // Beam ships against Postgres alone. Taken before anything reads the
    // ledger, so that the read which decides what is pending happens under the
    // lock -- under READ COMMITTED every statement after it sees what the
    // previous holder committed.
    if batch.get_database_backend() == DbBackend::Postgres {
        batch
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pg_advisory_xact_lock($1)",
                [MIGRATION_LOCK_KEY.into()],
            ))
            .await?;
    }
    let outcome = match M::get_pending_migrations_read_only(&batch).await {
        Ok(pending) => M::up(&batch, steps).await.map(|()| pending.len()),
        Err(ledger_error) => Err(ledger_error),
    };
    match outcome {
        Ok(pending) => batch.commit().await.map(|()| pending),
        Err(migration_error) => match batch.rollback().await {
            Ok(()) => Err(migration_error),
            Err(rollback_error) => Err(DbErr::Custom(format!(
                "{migration_error}; rolling the migration batch back also failed: {rollback_error}"
            ))),
        },
    }
}

/// `M` with `up` applied through [`up_all_or_nothing`] and a read-only
/// `status`; every other command is `M`'s own.
///
/// The `beam-migration` CLI runs `AllOrNothing::<Migrator>`. The CLI dispatches
/// on `MigratorTraitSelf`, whose blanket impl for every [`MigratorTrait`]
/// forwards to the static `up`, so overriding `up` here is what routes
/// `beam-migration up` through the shared path. `migrations` and
/// `migration_table_name` delegate to `M`, so the ledger and the migration list
/// are `M`'s. `status` reads the ledger without sea-orm's default `CREATE TABLE
/// IF NOT EXISTS`, so `beam-migration status` beside a starting server on a
/// fresh database cannot race its ledger creation (see
/// [`up_all_or_nothing`]). `down`, `fresh`, `refresh` and `reset` keep
/// sea-orm's per-migration transactions: the trait defaults call its internal
/// executor directly, not `up`. They are destructive operator commands, run
/// with the server stopped.
pub struct AllOrNothing<M>(PhantomData<fn() -> M>);

impl<M> AllOrNothing<M> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<M> Default for AllOrNothing<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl<M: MigratorTrait> MigratorTrait for AllOrNothing<M> {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        M::migrations()
    }

    fn migration_table_name() -> sea_orm::DynIden {
        M::migration_table_name()
    }

    async fn up<'c, C>(db: C, steps: Option<u32>) -> Result<(), DbErr>
    where
        C: IntoSchemaManagerConnection<'c>,
    {
        up_all_or_nothing::<M, C>(db, steps).await
    }

    async fn status<C>(db: &C) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        tracing::info!("Checking migration status");
        for migration in M::get_migration_with_status_read_only(db).await? {
            tracing::info!("Migration '{}'... {}", migration.name(), migration.status());
        }
        Ok(())
    }
}
