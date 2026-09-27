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

use beam_migration::{AllOrNothing, up_all_or_nothing};
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

/// Signalled by [`Gated::up`] once it is running inside the first migrator's
/// batch; the test waits for it before starting the second migrator.
static GATED_ENTERED: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Awaited by [`Gated::up`] before it creates its table; the test signals it
/// once the second migrator is observed waiting on the first.
static GATED_RELEASE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// The backend pid of the connection running the first migrator's batch.
static GATED_BATCH_PID: std::sync::OnceLock<i32> = std::sync::OnceLock::new();

/// Creates a table, but only once the test lets it. It holds its migrator's
/// batch open at a point the test chooses, so the second migrator is started
/// against a batch that is known to be in flight -- no timing guesswork.
struct Gated;

impl MigrationName for Gated {
    fn name(&self) -> &str {
        GATED_MIGRATION
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Gated {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        let row = connection
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("pg_backend_pid returned no row".to_string()))?;
        let pid: i32 = row.try_get("", "pid")?;
        GATED_BATCH_PID
            .set(pid)
            .map_err(|_| DbErr::Custom("the gated migration ran twice".to_string()))?;
        GATED_ENTERED.notify_one();
        GATED_RELEASE.notified().await;
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

struct GatedRelease;

impl MigratorTrait for GatedRelease {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Gated)]
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

/// Two migrators started against the same fresh database -- two pods of a
/// rolling upgrade, or `beam-migration up` beside a starting server -- must
/// both succeed, and the schema must be migrated exactly once.
///
/// The second migrator is started only while the first is provably mid-batch,
/// and the first is let go only once something is provably waiting on it.
/// Without the advisory lock the second reads the same empty ledger, blocks on
/// the first's uncommitted catalog rows, and fails with a duplicate-object
/// error the moment the first commits.
#[tokio::test]
async fn concurrent_migrators_apply_each_migration_once_and_both_succeed() {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

    let scoped = ScopedSchema::create("concurrent")
        .await
        .expect("create schema");
    // Outside the scoped pool, whose two connections the migrators hold.
    let observer = sea_orm::Database::connect(beam_test_support::postgres::database_url())
        .await
        .expect("connect the observer");

    let first_db = scoped.db();
    let first =
        tokio::spawn(
            async move { up_all_or_nothing::<GatedRelease, _>(first_db.as_ref(), None).await },
        );
    tokio::time::timeout(DEADLINE, GATED_ENTERED.notified())
        .await
        .expect("the first migrator reaches its migration");
    let batch_pid = *GATED_BATCH_PID
        .get()
        .expect("the gated migration recorded its pid");

    let second_db = scoped.db();
    let second = tokio::spawn(async move {
        up_all_or_nothing::<GatedRelease, _>(second_db.as_ref(), None).await
    });
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
    GATED_RELEASE.notify_one();

    let first = tokio::time::timeout(DEADLINE, first)
        .await
        .expect("the first migrator finishes")
        .expect("the first migrator task does not panic");
    let second = tokio::time::timeout(DEADLINE, second)
        .await
        .expect("the second migrator finishes")
        .expect("the second migrator task does not panic");
    assert!(
        first.is_ok(),
        "the first migrator must succeed, got {first:?}"
    );
    assert!(
        second.is_ok(),
        "the second migrator must wait for the first, then find nothing pending, got {second:?}"
    );

    let db = scoped.db();
    let db = db.as_ref();
    let tables = table_names(db, scoped.name()).await.expect("list tables");
    assert!(
        tables.contains(&GATED_TABLE.to_string()),
        "the migration must be committed, got {tables:?}"
    );
    let applied = GatedRelease::get_applied_migrations(db)
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

    drop(observer);
    scoped.drop_schema().await.expect("drop schema");
}
