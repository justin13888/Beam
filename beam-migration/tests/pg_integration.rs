//! The opt-in `pg-integration` tier for beam-migration: that the upgrade path
//! is all-or-nothing against a real Postgres.
//!
//! Compiled to nothing unless `--features pg-integration` is passed, so the
//! default `cargo test --workspace` still needs no infrastructure (NFR-201).
//! Run with `mise run rust:test:pg`.
//!
//! The migrators here are test-only: a batch whose last migration fails cannot
//! be staged with `beam_migration::Migrator`, whose migrations all succeed.
//! Each test runs in its own `ScopedSchema`, so the ledger and tables it
//! creates are invisible to every other test.
#![cfg(feature = "pg-integration")]

use beam_migration::{AllOrNothing, apply_pending, up_all_or_nothing};
use beam_test_support::postgres::{ScopedSchema, table_names};
use sea_orm_migration::prelude::*;

const ALPHA_TABLE: &str = "atomicity_alpha";
const BETA_TABLE: &str = "atomicity_beta";

/// Creates a table, so its effect is visible in the schema afterwards.
struct CreateTable {
    migration_name: &'static str,
    table: &'static str,
}

impl MigrationName for CreateTable {
    fn name(&self) -> &str {
        self.migration_name
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(
                r#"CREATE TABLE "{}" (id integer PRIMARY KEY)"#,
                self.table
            ))
            .await
            .map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(r#"DROP TABLE "{}""#, self.table))
            .await
            .map(|_| ())
    }
}

/// Fails in Postgres itself, the way a migration with a bad statement would.
struct Broken;

impl MigrationName for Broken {
    fn name(&self) -> &str {
        "m_test_000003_broken"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Broken {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE atomicity_table_that_does_not_exist ADD COLUMN x int")
            .await
            .map(|_| ())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

fn create_alpha() -> Box<dyn MigrationTrait> {
    Box::new(CreateTable {
        migration_name: "m_test_000001_create_alpha",
        table: ALPHA_TABLE,
    })
}

fn create_beta() -> Box<dyn MigrationTrait> {
    Box::new(CreateTable {
        migration_name: "m_test_000002_create_beta",
        table: BETA_TABLE,
    })
}

/// A release whose first migration succeeds and whose second fails.
struct HalfBrokenRelease;

impl MigratorTrait for HalfBrokenRelease {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![create_alpha(), Box::new(Broken)]
    }
}

/// A release whose migrations all succeed.
struct HealthyRelease;

impl MigratorTrait for HealthyRelease {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![create_alpha(), create_beta()]
    }
}

#[tokio::test]
async fn a_failing_migration_rolls_back_the_whole_pending_batch() {
    let scoped = ScopedSchema::create("atomic_fail")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let outcome = up_all_or_nothing::<HalfBrokenRelease, _>(db, None).await;

    assert!(
        outcome.is_err(),
        "the broken migration's error must reach the caller, got {outcome:?}"
    );
    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        !tables.contains(&ALPHA_TABLE.to_string()),
        "the migration before the failing one must not stay committed, got {tables:?}"
    );
    let applied = HalfBrokenRelease::get_applied_migrations(db)
        .await
        .expect("read the migration ledger");
    assert!(
        applied.is_empty(),
        "the ledger must record nothing from a failed batch, got {:?}",
        applied
            .iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>()
    );

    scoped.drop_schema().await.expect("drop schema");
}

/// `beam-migration up` runs `AllOrNothing::<Migrator>` through `run_cli`, which
/// calls `MigratorTraitSelf::up` on the value it is given. Driving the wrapper
/// through that same entry point, over a release whose second migration fails,
/// pins that the CLI's `up` is all-or-nothing and not sea-orm's per-migration
/// default.
#[tokio::test]
async fn the_cli_migrator_rolls_back_the_whole_pending_batch() {
    let scoped = ScopedSchema::create("atomic_cli")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let cli_migrator = AllOrNothing::<HalfBrokenRelease>::new();
    let outcome = sea_orm_migration::MigratorTraitSelf::up(&cli_migrator, db, None).await;

    assert!(
        outcome.is_err(),
        "the broken migration's error must reach the CLI, got {outcome:?}"
    );
    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        !tables.contains(&ALPHA_TABLE.to_string()),
        "the CLI must not leave the migration before the failing one committed, got {tables:?}"
    );
    let applied = sea_orm_migration::MigratorTraitSelf::get_applied_migrations(&cli_migrator, db)
        .await
        .expect("read the migration ledger");
    assert!(
        applied.is_empty(),
        "the ledger must record nothing from a failed CLI batch, got {:?}",
        applied
            .iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>()
    );

    scoped.drop_schema().await.expect("drop schema");
}

#[tokio::test]
async fn a_healthy_batch_commits_every_migration_and_its_ledger_row() {
    let scoped = ScopedSchema::create("atomic_ok")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    up_all_or_nothing::<HealthyRelease, _>(db, None)
        .await
        .expect("a batch of healthy migrations applies");

    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        tables.contains(&ALPHA_TABLE.to_string()) && tables.contains(&BETA_TABLE.to_string()),
        "every migration in the batch must be committed, got {tables:?}"
    );
    let pending = HealthyRelease::get_pending_migrations(db)
        .await
        .expect("read the migration ledger");
    assert!(
        pending.is_empty(),
        "every migration in the batch must be recorded as applied, still pending: {:?}",
        pending
            .iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>()
    );

    scoped.drop_schema().await.expect("drop schema");
}

const GATED_TABLE: &str = "concurrency_gated";
const GATED_MIGRATION: &str = "m_test_000001_gated";

/// The hand-off between a test and the one [`Gated`] migration it stages. Each
/// test owns its own, so tests running in parallel never signal each other.
struct Gate {
    /// Signalled by [`Gated::up`] once it is running inside the first
    /// migrator's batch; the test waits for it before starting the second
    /// migrator.
    entered: tokio::sync::Notify,
    /// Awaited by [`Gated::up`] before it creates its table; the test signals
    /// it once the second migrator is observed waiting on the first.
    release: tokio::sync::Notify,
    /// The backend pid of the connection running the first migrator's batch.
    batch_pid: std::sync::OnceLock<i32>,
}

impl Gate {
    const fn new() -> Self {
        Self {
            entered: tokio::sync::Notify::const_new(),
            release: tokio::sync::Notify::const_new(),
            batch_pid: std::sync::OnceLock::new(),
        }
    }
}

/// Creates a table, but only once the test lets it. It holds its migrator's
/// batch open at a point the test chooses, so the second migrator is started
/// against a batch that is known to be in flight -- no timing guesswork.
struct Gated(&'static Gate);

impl MigrationName for Gated {
    fn name(&self) -> &str {
        GATED_MIGRATION
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Gated {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let Self(gate) = self;
        let connection = manager.get_connection();
        let row = connection
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("pg_backend_pid returned no row".to_string()))?;
        let pid: i32 = row.try_get("", "pid")?;
        gate.batch_pid
            .set(pid)
            .map_err(|_| DbErr::Custom("the gated migration ran twice".to_string()))?;
        gate.entered.notify_one();
        gate.release.notified().await;
        connection
            .execute_unprepared(&format!(
                r#"CREATE TABLE "{GATED_TABLE}" (id integer PRIMARY KEY)"#
            ))
            .await
            .map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(r#"DROP TABLE "{GATED_TABLE}""#))
            .await
            .map(|_| ())
    }
}

static CONCURRENT_GATE: Gate = Gate::new();

/// The gated release `concurrent_migrators_apply_each_migration_once_and_both_succeed` runs.
struct GatedRelease;

impl MigratorTrait for GatedRelease {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Gated(&CONCURRENT_GATE))]
    }
}

static STARTUP_GATE: Gate = Gate::new();

/// The gated release `server_startup_beside_an_in_flight_cli_migrator_waits_then_applies_nothing`
/// runs.
struct StartupGatedRelease;

impl MigratorTrait for StartupGatedRelease {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Gated(&STARTUP_GATE))]
    }
}

/// Whether any backend is waiting on a lock the first migrator's batch holds.
async fn anything_waits_on(
    observer: &sea_orm::DatabaseConnection,
    batch_pid: i32,
) -> Result<bool, DbErr> {
    let row = observer
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) AS waiting FROM pg_stat_activity \
             WHERE $1 = ANY(pg_blocking_pids(pid))",
            [batch_pid.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("count(*) returned no row".to_string()))?;
    let waiting: i64 = row.try_get("", "waiting")?;
    Ok(waiting > 0)
}

const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs `first` until its gated migration is provably mid-batch, then starts
/// `second`, lets the first go only once something is provably waiting on it,
/// and returns both outcomes.
async fn race_second_against_in_flight_first<F, S, T, U>(
    gate: &'static Gate,
    first: F,
    second: S,
) -> (T, U)
where
    F: std::future::Future<Output = T> + Send + 'static,
    S: std::future::Future<Output = U> + Send + 'static,
    T: Send + 'static,
    U: Send + 'static,
{
    // Outside the scoped pool, whose two connections the migrators hold.
    let observer = sea_orm::Database::connect(beam_test_support::postgres::database_url())
        .await
        .expect("connect the observer");

    let first = tokio::spawn(first);
    tokio::time::timeout(DEADLINE, gate.entered.notified())
        .await
        .expect("the first migrator reaches its migration");
    let batch_pid = *gate
        .batch_pid
        .get()
        .expect("the gated migration recorded its pid");

    let second = tokio::spawn(second);
    // Polled on the database's own answer rather than timed: each round trip
    // is the pacing, and the deadline only bounds a hang.
    tokio::time::timeout(DEADLINE, async {
        while !anything_waits_on(&observer, batch_pid)
            .await
            .expect("read pg_stat_activity")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second migrator waits on the first's batch");
    gate.release.notify_one();

    let first = tokio::time::timeout(DEADLINE, first)
        .await
        .expect("the first migrator finishes")
        .expect("the first migrator task does not panic");
    let second = tokio::time::timeout(DEADLINE, second)
        .await
        .expect("the second migrator finishes")
        .expect("the second migrator task does not panic");
    drop(observer);
    (first, second)
}

/// Asserts the gated migration committed and is in `M`'s ledger exactly once.
async fn assert_gated_migration_applied_once<M: MigratorTrait>(scoped: &ScopedSchema) {
    let db = scoped.db();
    let db = db.as_ref();
    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        tables.contains(&GATED_TABLE.to_string()),
        "the migration must be committed, got {tables:?}"
    );
    let applied = M::get_applied_migrations(db)
        .await
        .expect("read the migration ledger")
        .iter()
        .map(|migration| migration.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        applied,
        vec![GATED_MIGRATION.to_string()],
        "the ledger must record the migration exactly once"
    );
}

/// Two migrators started against the same fresh database -- two pods of a
/// rolling upgrade, or `beam-migration up` beside a starting server -- must
/// both succeed, and the schema must be migrated exactly once.
///
/// Without the advisory lock the second reads the same empty ledger, blocks on
/// the first's uncommitted catalog rows, and fails with a duplicate-object
/// error the moment the first commits.
#[tokio::test]
async fn concurrent_migrators_apply_each_migration_once_and_both_succeed() {
    let scoped = ScopedSchema::create("concurrent")
        .await
        .expect("create schema");

    let first_db = scoped.db();
    let second_db = scoped.db();
    let (first, second) = race_second_against_in_flight_first(
        &CONCURRENT_GATE,
        async move { up_all_or_nothing::<GatedRelease, _>(first_db.as_ref(), None).await },
        async move { up_all_or_nothing::<GatedRelease, _>(second_db.as_ref(), None).await },
    )
    .await;
    assert!(
        first.is_ok(),
        "the first migrator must succeed, got {first:?}"
    );
    assert!(
        second.is_ok(),
        "the second migrator must wait for the first, then find nothing pending, got {second:?}"
    );
    assert_gated_migration_applied_once::<GatedRelease>(&scoped).await;

    scoped.drop_schema().await.expect("drop schema");
}

/// `beam-server` starting on a fresh database while `beam-migration up` is
/// mid-batch -- the operator ran the CLI beside a pod that was just scheduled.
/// The server side runs [`apply_pending`], the function `beam-server`'s `main`
/// calls, so this pins the whole startup sequence and not just the batch
/// inside it.
///
/// The CLI's batch has created the migration ledger but not committed it. Any
/// ledger DDL the server issued before taking the advisory lock -- sea-orm's
/// `get_pending_migrations` runs `CREATE TABLE IF NOT EXISTS` -- would block on
/// those uncommitted catalog rows and fail with a unique violation the moment
/// the CLI commits. The server must instead wait on the lock, then find
/// nothing pending and report that it applied nothing.
#[tokio::test]
async fn server_startup_beside_an_in_flight_cli_migrator_waits_then_applies_nothing() {
    let scoped = ScopedSchema::create("startup")
        .await
        .expect("create schema");

    let cli_db = scoped.db();
    let server_db = scoped.db();
    let (cli, server) = race_second_against_in_flight_first(
        &STARTUP_GATE,
        async move {
            let cli_migrator = AllOrNothing::<StartupGatedRelease>::new();
            sea_orm_migration::MigratorTraitSelf::up(&cli_migrator, cli_db.as_ref(), None).await
        },
        async move { apply_pending::<StartupGatedRelease>(server_db.as_ref()).await },
    )
    .await;
    assert!(cli.is_ok(), "the CLI migrator must succeed, got {cli:?}");
    assert_eq!(
        server,
        Ok(0),
        "the starting server must wait for the CLI, then apply nothing"
    );
    assert_gated_migration_applied_once::<StartupGatedRelease>(&scoped).await;

    scoped.drop_schema().await.expect("drop schema");
}

/// `beam-migration status` is read-only: on a database nothing has migrated
/// yet it reports every migration pending without creating the ledger table.
/// sea-orm's own `status` runs the ledger's `CREATE TABLE IF NOT EXISTS`
/// outside any lock, which would race a migrator starting on the same fresh
/// database exactly as a pre-lock ledger read in the server would.
#[tokio::test]
async fn the_cli_status_command_creates_nothing() {
    let scoped = ScopedSchema::create("status").await.expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let cli_migrator = AllOrNothing::<HealthyRelease>::new();
    sea_orm_migration::MigratorTraitSelf::status(&cli_migrator, db)
        .await
        .expect("status reads an absent ledger as all pending");

    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        tables.is_empty(),
        "status must not create the ledger or anything else, got {tables:?}"
    );

    scoped.drop_schema().await.expect("drop schema");
}
