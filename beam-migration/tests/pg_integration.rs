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

/// `beam-server`'s startup on a fresh database, with nothing else migrating:
/// [`apply_pending`] applies the whole release and reports every migration in
/// it, which is the count `beam-server` logs.
#[tokio::test]
async fn server_startup_on_a_fresh_database_applies_and_counts_every_migration() {
    let scoped = ScopedSchema::create("startup_fresh")
        .await
        .expect("create schema");
    let db = scoped.db();

    let applied = apply_pending::<HealthyRelease>(db.as_ref()).await;

    assert_eq!(
        applied,
        Ok(HealthyRelease::migrations().len()),
        "a fresh database has the whole release pending, and startup applies all of it"
    );
    let tables = table_names(db.as_ref(), scoped.name())
        .await
        .expect("list tables");
    assert!(
        tables.contains(&ALPHA_TABLE.to_string()) && tables.contains(&BETA_TABLE.to_string()),
        "every migration startup counted must be committed, got {tables:?}"
    );
    let pending = HealthyRelease::get_pending_migrations_read_only(db.as_ref())
        .await
        .expect("read the migration ledger");
    assert!(
        pending.is_empty(),
        "every migration startup counted must be recorded as applied, still pending: {:?}",
        pending
            .iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>()
    );

    drop(db);
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

/// Whether the second migrator is waiting on a lock the first migrator's batch
/// holds.
///
/// Scoped to `application_name`, the scoped schema's name, which every backend
/// on that schema's pool reports. The first migrator's connection is the
/// blocker itself, so the only backend that can match is the second
/// migrator's. Counting *any* backend blocked by the batch would not do: every
/// lock-taking test in this binary queues on the same `MIGRATION_LOCK_KEY`, so
/// a parallel test's migrator could satisfy the check before the second
/// migrator issued anything -- the first would then be released early, the
/// race never staged, and the test pass with the bug it pins reintroduced.
async fn second_migrator_waits_on(
    observer: &sea_orm::DatabaseConnection,
    application_name: &str,
    batch_pid: i32,
) -> Result<bool, DbErr> {
    let row = observer
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) AS waiting FROM pg_stat_activity \
             WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid))",
            [application_name.into(), batch_pid.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("count(*) returned no row".to_string()))?;
    let waiting: i64 = row.try_get("", "waiting")?;
    Ok(waiting > 0)
}

const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs `first` until its gated migration is provably mid-batch, then starts
/// `second`, lets the first go only once `second` is provably waiting on it,
/// and returns both outcomes. Both must run on `scoped`'s pool, whose two
/// connections they hold.
async fn race_second_against_in_flight_first<F, S, T, U>(
    gate: &'static Gate,
    scoped: &ScopedSchema,
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
        while !second_migrator_waits_on(&observer, scoped.name(), batch_pid)
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
        &scoped,
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
        &scoped,
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

/// Issue #182's migration: duplicate default-edition movie entries -- one per
/// file, which the old `NULL`s-distinct index let through -- are merged into
/// the oldest, their files repointed; the recreated index then refuses a
/// second default-edition entry; and a multi-episode range cannot sit on a
/// file that is not an episode file.
#[tokio::test]
async fn the_classifier_migration_merges_duplicate_entries_and_constrains_what_it_adds() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("classifier_v2")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let migrations = beam_migration::Migrator::migrations();
    let this_one = migrations
        .iter()
        .position(|m| m.name() == "m20260929_000001_classifier_v2")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(this_one as u32))
        .await
        .expect("every earlier migration applies");

    // A movie with two default-edition entries (the older created first), one
    // Director's Cut entry, and a file behind each.
    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie', 'movie|', now(), now())",
        "INSERT INTO shows (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000c', 'Grey''s Anatomy', 'grey s anatomy|', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) VALUES \
         ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, now() - interval '2 days'), \
         ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, now() - interval '1 day'), \
         ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', 'Director''s Cut', now())",
        "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                            scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-000000000001', \
          '00000000-0000-0000-0000-00000000000a', '/videos/a.mkv', 1, 1, now(), now()), \
         ('00000000-0000-0000-0000-0000000000f2', '00000000-0000-0000-0000-000000000002', \
          '00000000-0000-0000-0000-00000000000a', '/videos/b.mkv', 1, 2, now(), now()), \
         ('00000000-0000-0000-0000-0000000000f3', '00000000-0000-0000-0000-000000000003', \
          '00000000-0000-0000-0000-00000000000a', '/videos/c.mkv', 1, 3, now(), now())",
    ];
    for sql in seed {
        db.execute_unprepared(sql)
            .await
            .expect("seed pre-migration rows");
    }

    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the classifier migration applies over duplicate entries");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT id::text AS v FROM movie_entries ORDER BY created_at").await,
        vec![
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003"
        ],
        "the newer default-edition duplicate is merged away; the edition stays"
    );
    assert_eq!(
        text("SELECT movie_entry_id::text AS v FROM files ORDER BY file_path").await,
        vec![
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003"
        ],
        "the duplicate's file now belongs to the surviving entry"
    );
    assert_eq!(
        text("SELECT classifier_version::text AS v FROM files GROUP BY classifier_version").await,
        vec!["0"],
        "existing rows are marked as classified before versions existed"
    );
    assert_eq!(
        text(
            "SELECT identity_key_version::text AS v FROM movies \
             UNION ALL SELECT identity_key_version::text FROM shows"
        )
        .await,
        vec!["0", "0"],
        "keys stored before versions existed are marked for re-derivation"
    );

    assert!(
        db.execute_unprepared(
            "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) \
             VALUES (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                     '00000000-0000-0000-0000-00000000000b', NULL, now())",
        )
        .await
        .is_err(),
        "a second default-edition entry must be refused"
    );
    assert!(
        db.execute_unprepared(
            "UPDATE files SET last_episode_number = 2 \
              WHERE id = '00000000-0000-0000-0000-0000000000f1'",
        )
        .await
        .is_err(),
        "a movie file cannot carry an episode range"
    );

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #182's migration reverses: `down()` drops the columns it added (and the
/// `CHECK` that reads one of them) and restores the unique index's old,
/// `NULL`s-distinct semantics; `up()` then applies again over the result.
#[tokio::test]
async fn the_classifier_migration_rolls_back_and_reapplies() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("classifier_v2_down")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    // Applied up to and including this migration, so rolling back one step
    // reverts exactly it, however many migrations come after it.
    let through_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20260929_000001_classifier_v2")
        .expect("the migration is registered")
        + 1;
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(through_this_one as u32))
        .await
        .expect("every migration through this one applies");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    let added_columns = "SELECT (table_name || '.' || column_name)::text AS v \
                           FROM information_schema.columns \
                          WHERE table_schema = current_schema() \
                            AND table_name IN ('files', 'movies', 'shows') \
                            AND column_name IN ('classifier_version', 'last_episode_number', \
                                                'identity_key_version') \
                          ORDER BY 1";
    let nulls_not_distinct = "SELECT i.indnullsnotdistinct::text AS v FROM pg_index i \
                               JOIN pg_class c ON c.oid = i.indexrelid \
                               JOIN pg_namespace n ON n.oid = c.relnamespace \
                              WHERE c.relname = 'idx_movie_entries_unique' \
                                AND n.nspname = current_schema()";
    assert_eq!(
        text(added_columns).await,
        vec![
            "files.classifier_version",
            "files.last_episode_number",
            "movies.identity_key_version",
            "shows.identity_key_version"
        ]
    );
    assert_eq!(text(nulls_not_distinct).await, vec!["true"]);

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the classifier migration rolls back");

    assert!(
        text(added_columns).await.is_empty(),
        "down() drops every column it added"
    );
    assert_eq!(
        text(nulls_not_distinct).await,
        vec!["false"],
        "down() restores the NULLs-distinct unique index"
    );
    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO movies (id, title, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) \
         VALUES (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, now()), \
                (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, now())",
    ];
    for sql in seed {
        db.execute_unprepared(sql)
            .await
            .expect("the rolled-back schema takes what it took before the migration");
    }

    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");
    assert_eq!(
        text("SELECT count(*)::text AS v FROM movie_entries").await,
        vec!["1"],
        "reapplying merges the duplicates the rolled-back schema let in"
    );
    assert_eq!(text(nulls_not_distinct).await, vec!["true"]);

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #181's migration: a path stored more than once keeps one row -- the
/// present, `known` one with the most progress -- every user's newest
/// progress moves onto it, the others' streams go with them, and a second row
/// for a path is refused from then on. A path stored once is untouched.
#[tokio::test]
async fn the_unique_path_migration_merges_duplicate_rows_and_their_progress() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("files_unique_path")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let migrations = beam_migration::Migrator::migrations();
    let this_one = migrations
        .iter()
        .position(|m| m.name() == "m20261001_000001_files_unique_path")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(this_one as u32))
        .await
        .expect("every earlier migration applies");

    // Three rows at /videos/a.mkv:
    //   a1 -- missing, with progress for both users (the newest for user 1)
    //   a2 -- present and known, with progress for user 2 (their newest)
    //   a3 -- present and unknown, no progress
    // a2 is kept: present beats missing, known beats unknown. One row at
    // /videos/b.mkv, with progress, is left alone.
    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO users (id, display_name, is_admin, oidc_issuer, oidc_subject, created_at, \
                            updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000c1', 'one', false, 'iss', 'one', now(), now()), \
         ('00000000-0000-0000-0000-0000000000c2', 'two', false, 'iss', 'two', now(), now())",
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie', 'movie|', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) \
         VALUES ('00000000-0000-0000-0000-00000000000e', '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, now())",
        "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                            file_status, missing_since, scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000a1', '00000000-0000-0000-0000-00000000000e', \
          '00000000-0000-0000-0000-00000000000a', '/videos/a.mkv', 1, 1, 'known', now(), now(), \
          now()), \
         ('00000000-0000-0000-0000-0000000000a2', '00000000-0000-0000-0000-00000000000e', \
          '00000000-0000-0000-0000-00000000000a', '/videos/a.mkv', 1, 2, 'known', NULL, now(), \
          now() - interval '1 day'), \
         ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-00000000000e', \
          '00000000-0000-0000-0000-00000000000a', '/videos/b.mkv', 1, 4, 'known', NULL, now(), \
          now())",
        "INSERT INTO files (id, library_id, file_path, file_size, hash_xxh3, file_status, \
                            scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000a3', '00000000-0000-0000-0000-00000000000a', \
          '/videos/a.mkv', 1, 3, 'unknown', now(), now())",
        "INSERT INTO media_streams (id, file_id, stream_index, stream_type, codec) VALUES \
         ('00000000-0000-0000-0000-0000000000d1', '00000000-0000-0000-0000-0000000000a1', 0, \
          'video', 'h264')",
        "INSERT INTO playback_progress (id, user_id, file_id, position_secs, completed, updated_at) \
         VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000a1', 100, false, now()), \
         ('00000000-0000-0000-0000-0000000000f2', '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000a1', 200, false, now() - interval '2 days'), \
         ('00000000-0000-0000-0000-0000000000f3', '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000a2', 300, false, now() - interval '1 day'), \
         ('00000000-0000-0000-0000-0000000000f4', '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000b1', 400, false, now())",
    ];
    for sql in seed {
        db.execute_unprepared(sql)
            .await
            .expect("seed pre-migration rows");
    }

    // This migration alone: the later `watch_state` migration replaces the
    // `playback_progress` table this test reads back.
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("the unique-path migration applies over duplicate rows");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT id::text AS v FROM files ORDER BY file_path").await,
        vec![
            "00000000-0000-0000-0000-0000000000a2",
            "00000000-0000-0000-0000-0000000000b1"
        ],
        "the present, known row is kept; the untouched path keeps its row"
    );
    assert_eq!(
        text(
            "SELECT (user_id::text || ' ' || file_id::text || ' ' || position_secs::text) AS v \
               FROM playback_progress ORDER BY user_id, file_id"
        )
        .await,
        vec![
            "00000000-0000-0000-0000-0000000000c1 00000000-0000-0000-0000-0000000000a2 100",
            "00000000-0000-0000-0000-0000000000c1 00000000-0000-0000-0000-0000000000b1 400",
            "00000000-0000-0000-0000-0000000000c2 00000000-0000-0000-0000-0000000000a2 300",
        ],
        "each user's newest progress on the path moves to the kept row"
    );
    assert!(
        text("SELECT id::text AS v FROM media_streams")
            .await
            .is_empty(),
        "a merged-away row's streams go with it"
    );
    assert!(
        db.execute_unprepared(
            "INSERT INTO files (id, library_id, file_path, file_size, hash_xxh3, file_status, \
                                scanned_at, updated_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', '/videos/b.mkv', 1, 9, \
              'unknown', now(), now())",
        )
        .await
        .is_err(),
        "a second row for a path must be refused, whatever its hash"
    );

    // Reversible: the old `(hash, path)` index comes back, and so does what
    // it allowed.
    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the unique-path migration rolls back");
    db.execute_unprepared(
        "INSERT INTO files (id, library_id, file_path, file_size, hash_xxh3, file_status, \
                            scanned_at, updated_at) VALUES \
         (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', '/videos/b.mkv', 1, 9, \
          'unknown', now(), now())",
    )
    .await
    .expect("the rolled-back schema takes a second row at a path under another hash");

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #184's migration: one provider id pins one title, a sidecar subtitle
/// goes with its video file and is held to the text formats, and `down()`
/// takes all of it away so `up()` can apply again.
#[tokio::test]
async fn the_nfo_sidecar_migration_constrains_what_it_adds_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("nfo_sidecars")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    // Applied up to and including this migration, so rolling back one step
    // reverts exactly it, however many migrations come after it.
    let through_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261002_000001_nfo_sidecars")
        .expect("the migration is registered")
        + 1;
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(through_this_one as u32))
        .await
        .expect("every migration through this one applies");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO movies (id, title, identity_key, pinned_ref, pin_source, created_at, \
                             updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'The Matrix', 'the matrix|1999', 'tmdb:603', \
          'nfo', now(), now()), \
         ('00000000-0000-0000-0000-00000000000c', 'Heat', 'heat|1995', NULL, NULL, now(), now()), \
         ('00000000-0000-0000-0000-00000000000d', 'Alien', 'alien|1979', NULL, NULL, now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) VALUES \
         ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, now())",
        "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                            scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-000000000001', \
          '00000000-0000-0000-0000-00000000000a', '/videos/m.mkv', 1, 1, now(), now())",
        "INSERT INTO sidecar_subtitles (id, file_id, library_id, path, format, language, \
                                        size_bytes, created_at, updated_at) VALUES \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000f1', \
          '00000000-0000-0000-0000-00000000000a', '/videos/m.en.srt', 'srt', 'eng', 10, now(), now())",
        "INSERT INTO applied_nfos (id, library_id, path, size_bytes, content_hash, created_at, \
                                   updated_at) VALUES \
         (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', '/videos/movie.nfo', 2, \
          'c0ffee', now(), now())",
    ];
    for sql in seed {
        db.execute_unprepared(sql).await.expect("seed rows");
    }

    for (refused, why) in [
        (
            "UPDATE movies SET pinned_ref = 'tmdb:603', pin_source = 'admin' \
              WHERE id = '00000000-0000-0000-0000-00000000000c'",
            "one provider id pins one movie",
        ),
        (
            "UPDATE movies SET pinned_ref = 'tmdb:949' \
              WHERE id = '00000000-0000-0000-0000-00000000000c'",
            "a pin says who set it",
        ),
        (
            "UPDATE movies SET pin_source = 'nfo' \
              WHERE id = '00000000-0000-0000-0000-00000000000d'",
            "no source without a pin",
        ),
        (
            "UPDATE movies SET pinned_ref = 'tmdb:949', pin_source = 'user' \
              WHERE id = '00000000-0000-0000-0000-00000000000c'",
            "a pin is set by an NFO or an administrator",
        ),
        (
            "INSERT INTO sidecar_subtitles (id, file_id, library_id, path, format, size_bytes, \
                                            created_at, updated_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000f1', \
              '00000000-0000-0000-0000-00000000000a', '/videos/m.sup', 'sup', 1, now(), now())",
            "an image-based subtitle format is refused",
        ),
        (
            "INSERT INTO sidecar_subtitles (id, file_id, library_id, path, format, size_bytes, \
                                            created_at, updated_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000f1', \
              '00000000-0000-0000-0000-00000000000a', '/videos/m.en.srt', 'srt', 1, now(), now())",
            "one row per subtitle path",
        ),
        (
            "INSERT INTO applied_nfos (id, library_id, path, size_bytes, content_hash, \
                                       created_at, updated_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', '/videos/movie.nfo', \
              2, 'c0ffee', now(), now())",
            "one record per NFO path",
        ),
        (
            "INSERT INTO applied_nfos (id, library_id, path, size_bytes, content_hash, \
                                       created_at, updated_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000ff', '/videos/other.nfo', \
              2, 'c0ffee', now(), now())",
            "an NFO record belongs to a library",
        ),
        (
            "UPDATE files SET container_tags = '[\"show\"]'::jsonb \
              WHERE id = '00000000-0000-0000-0000-0000000000f1'",
            "a file's container tags are a JSON object",
        ),
    ] {
        assert!(db.execute_unprepared(refused).await.is_err(), "{why}");
    }
    db.execute_unprepared(
        "UPDATE files SET container_tags = '{\"show\": \"The Office\", \"season\": 2}'::jsonb \
          WHERE id = '00000000-0000-0000-0000-0000000000f1'",
    )
    .await
    .expect("a file stores the tags its probe read");
    db.execute_unprepared(
        "UPDATE movies SET pinned_ref = NULL, pin_source = NULL \
          WHERE id = '00000000-0000-0000-0000-00000000000b'",
    )
    .await
    .expect("any number of titles may be unpinned");

    db.execute_unprepared("DELETE FROM files WHERE id = '00000000-0000-0000-0000-0000000000f1'")
        .await
        .expect("delete the video file");
    assert_eq!(
        text("SELECT count(*)::text AS v FROM sidecar_subtitles").await,
        vec!["0"],
        "a sidecar subtitle goes with its video file"
    );

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert!(
        text(
            "SELECT (table_name || '.' || column_name)::text AS v \
               FROM information_schema.columns \
              WHERE table_schema = current_schema() \
                AND (column_name IN ('pinned_ref', 'pin_source', 'container_tags') \
                     OR table_name IN ('sidecar_subtitles', 'applied_nfos'))"
        )
        .await
        .is_empty(),
        "down() drops the tables and every column it added"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #189's migration: every codec the indexer stored in one of its old
/// spellings reads back as FFmpeg names it, subtitles gain their
/// hearing-impaired flag, and the never-read `is_primary` columns go --
/// coming back under `down()` as the indexer wrote them, so `up()` applies
/// again.
#[tokio::test]
async fn the_tracks_migration_renames_codecs_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("tracks_subtitles")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261008_000001_tracks_subtitles")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(this_one as u32))
        .await
        .expect("every earlier migration applies");

    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie', 'movie|', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) \
         VALUES ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, true, now())",
        "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                            is_primary, scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-000000000001', \
          '00000000-0000-0000-0000-00000000000a', '/videos/m.mkv', 1, 1, true, now(), now())",
    ];
    for sql in seed {
        db.execute_unprepared(sql).await.expect("seed rows");
    }
    // Each spelling the indexer wrote before, beside the FFmpeg name it
    // becomes: video and audio as FFmpeg's codec id `Debug` name, subtitles
    // as the prober's display names.
    let codecs = [
        ("video", "H264", "h264"),
        ("video", "HEVC", "hevc"),
        ("video", "MPEG2VIDEO", "mpeg2video"),
        ("audio", "EAC3", "eac3"),
        ("audio", "TRUEHD", "truehd"),
        ("audio", "PCM_S16LE", "pcm_s16le"),
        // Two whose `Debug` name lower-cased is not FFmpeg's name.
        ("video", "XM4", "4xm"),
        ("audio", "ACELP_KELVIN", "acelp.kelvin"),
        ("subtitle", "SubRip", "subrip"),
        ("subtitle", "ASS/SSA", "ass"),
        ("subtitle", "WebVTT", "webvtt"),
        (
            "subtitle",
            "Other(\"hdmv_pgs_subtitle\")",
            "hdmv_pgs_subtitle",
        ),
        ("subtitle", "Unknown", "none"),
        ("subtitle", "subrip", "subrip"),
    ];
    for (index, (kind, stored, _)) in codecs.iter().enumerate() {
        db.execute_unprepared(&format!(
            "INSERT INTO media_streams (id, file_id, stream_index, stream_type, codec, \
                                        is_default, is_forced) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000f1', {index}, '{kind}', \
              '{stored}', false, false)"
        ))
        .await
        .expect("seed a stream");
    }

    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("the migration applies");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT codec AS v FROM media_streams ORDER BY stream_index").await,
        codecs
            .iter()
            .map(|(_, _, renamed)| String::from(*renamed))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        text("SELECT DISTINCT is_hearing_impaired::text AS v FROM media_streams").await,
        vec!["false"],
        "an existing stream is not hearing-impaired until it is probed again"
    );
    let primary_columns = "SELECT (table_name || '.' || column_name)::text AS v \
                             FROM information_schema.columns \
                            WHERE table_schema = current_schema() \
                              AND column_name = 'is_primary' \
                            ORDER BY 1";
    assert!(text(primary_columns).await.is_empty(), "is_primary is gone");

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert_eq!(
        text(primary_columns).await,
        vec!["files.is_primary", "movie_entries.is_primary"]
    );
    assert_eq!(
        text(
            "SELECT is_primary::text AS v FROM files \
             UNION ALL SELECT is_primary::text FROM movie_entries"
        )
        .await,
        vec!["true", "true"],
        "the rows read back as the indexer wrote them"
    );
    assert_eq!(
        text(
            "SELECT column_default::text AS v FROM information_schema.columns \
              WHERE table_schema = current_schema() AND column_name = 'is_primary'"
        )
        .await,
        vec!["false", "false"],
        "under the default they had before"
    );
    assert!(
        text(
            "SELECT column_name::text AS v FROM information_schema.columns \
              WHERE table_schema = current_schema() AND column_name = 'is_hearing_impaired'"
        )
        .await
        .is_empty()
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #228's migration: a row indexed before it reads as having no
/// identity, a row holds an inode and a ctime together or neither, and
/// `down()` takes both columns away so `up()` can apply again.
#[tokio::test]
async fn the_change_identity_migration_keeps_old_rows_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("files_change_identity")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let before_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261007_000001_files_change_identity")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(before_this_one as u32))
        .await
        .expect("every migration before this one applies");
    // A row written before the migration, as an existing install has one.
    for sql in [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO files (id, library_id, file_path, file_size, hash_xxh3, file_status, \
                            scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-00000000000a', \
          '/videos/m.mkv', 1, 1, 'unknown', now(), now())",
    ] {
        db.execute_unprepared(sql).await.expect("seed rows");
    }
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("the migration applies");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT (inode IS NULL AND ctime IS NULL)::text AS v FROM files").await,
        vec!["true"],
        "an existing row has no identity until a scan records it"
    );
    for (refused, why) in [
        (
            "UPDATE files SET inode = 42 WHERE id = '00000000-0000-0000-0000-0000000000f1'",
            "an inode without a ctime",
        ),
        (
            "UPDATE files SET ctime = now() WHERE id = '00000000-0000-0000-0000-0000000000f1'",
            "a ctime without an inode",
        ),
    ] {
        assert!(db.execute_unprepared(refused).await.is_err(), "{why}");
    }
    db.execute_unprepared(
        "UPDATE files SET inode = -1, ctime = now() \
          WHERE id = '00000000-0000-0000-0000-0000000000f1'",
    )
    .await
    .expect("an inode and a ctime are recorded together");

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert!(
        text(
            "SELECT column_name::text AS v FROM information_schema.columns \
              WHERE table_schema = current_schema() AND table_name = 'files' \
                AND column_name IN ('inode', 'ctime')"
        )
        .await
        .is_empty(),
        "down() drops both columns"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #185's migration: an existing enrichment row gains no locks, the
/// column holds only the names Beam knows, and `down()` takes the column and
/// its index away so `up()` can apply again.
#[tokio::test]
async fn the_enrichment_locks_migration_defaults_constrains_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("enrichment_locks")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let before_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261009_000001_enrichment_locks")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(before_this_one as u32))
        .await
        .expect("every migration before this one applies");
    for sql in [
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Heat', 'heat|1995', now(), now())",
        "INSERT INTO metadata_enrichment (id, movie_id, status, attempts, force_refresh, \
                                          created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000e1', '00000000-0000-0000-0000-00000000000b', \
          'enriched', 0, false, now(), now())",
    ] {
        db.execute_unprepared(sql).await.expect("seed rows");
    }
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("this migration applies over existing rows");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT array_to_string(locked_fields, ',') AS v FROM metadata_enrichment").await,
        vec![String::new()],
        "an existing row locks nothing"
    );
    assert!(
        db.execute_unprepared("UPDATE metadata_enrichment SET locked_fields = ARRAY['tmdb_id']")
            .await
            .is_err(),
        "an external id is not a lockable field"
    );
    db.execute_unprepared(
        "UPDATE metadata_enrichment SET locked_fields = \
         ARRAY['title', 'original_title', 'description', 'year', 'release_date', 'runtime', \
               'poster', 'backdrop', 'rating', 'genres']",
    )
    .await
    .expect("every known field locks");
    assert_eq!(
        text(
            "SELECT indexname::text AS v FROM pg_indexes \
              WHERE schemaname = current_schema() \
                AND indexname IN ('idx_metadata_enrichment_list', \
                                  'idx_metadata_enrichment_recent') \
              ORDER BY 1"
        )
        .await,
        vec![
            "idx_metadata_enrichment_list",
            "idx_metadata_enrichment_recent"
        ],
        "the list's order is indexed with and without a status filter"
    );

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert!(
        text(
            "SELECT column_name::text AS v FROM information_schema.columns \
              WHERE table_schema = current_schema() AND column_name = 'locked_fields' \
             UNION ALL \
             SELECT indexname::text AS v FROM pg_indexes \
              WHERE schemaname = current_schema() \
                AND indexname IN ('idx_metadata_enrichment_list', \
                                  'idx_metadata_enrichment_recent')"
        )
        .await
        .is_empty(),
        "down() drops the column and both indexes"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #188's migration: every user's per-file progress merges into one
/// row per title -- the newest row's place and file (a tie to the larger file
/// id), played if any row had finished, a play per finished row -- a finished
/// multi-episode file plays its whole run, a row with no title is dropped
/// but one on a missing file is kept, positions and durations no player
/// could produce are cleaned, and `down()` puts each row back on the file it
/// last played.
#[tokio::test]
async fn the_watch_state_migration_merges_progress_per_title_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("watch_state")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let before_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261011_000001_watch_state")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(before_this_one as u32))
        .await
        .expect("every migration before this one applies");

    // Movie b1 has two editions, each with a file (f1, f2); movie b2 has one
    // (f3), which the indexer has marked missing (#179). Season 1 of show d1
    // has episodes 1-3 (d3, d4, d5): episode 1 has a file of its own (f4)
    // and one holding episodes 1-3 (f6); episodes 2 and 3 have one each (f7,
    // f8). f5 belongs to no title.
    let seed = [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO users (id, display_name, is_admin, oidc_issuer, oidc_subject, created_at, \
                            updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000c1', 'one', false, 'iss', 'one', now(), now()), \
         ('00000000-0000-0000-0000-0000000000c2', 'two', false, 'iss', 'two', now(), now()), \
         ('00000000-0000-0000-0000-0000000000c3', 'three', false, 'iss', 'three', now(), now())",
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000b1', 'One', 'one|', now(), now()), \
         ('00000000-0000-0000-0000-0000000000b2', 'Two', 'two|', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) VALUES \
         ('00000000-0000-0000-0000-0000000000e1', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-0000000000b1', NULL, now()), \
         ('00000000-0000-0000-0000-0000000000e2', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-0000000000b1', 'Director''s Cut', now()), \
         ('00000000-0000-0000-0000-0000000000e3', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-0000000000b2', NULL, now())",
        "INSERT INTO shows (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000d1', 'Show', 'show|', now(), now())",
        "INSERT INTO seasons (id, show_id, season_number) VALUES \
         ('00000000-0000-0000-0000-0000000000d2', '00000000-0000-0000-0000-0000000000d1', 1)",
        "INSERT INTO episodes (id, season_id, episode_number, title, created_at) VALUES \
         ('00000000-0000-0000-0000-0000000000d3', '00000000-0000-0000-0000-0000000000d2', 1, \
          'Pilot', now()), \
         ('00000000-0000-0000-0000-0000000000d4', '00000000-0000-0000-0000-0000000000d2', 2, \
          'Two', now()), \
         ('00000000-0000-0000-0000-0000000000d5', '00000000-0000-0000-0000-0000000000d2', 3, \
          'Three', now())",
        "INSERT INTO files (id, movie_entry_id, episode_id, library_id, file_path, file_size, \
                            hash_xxh3, file_status, scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-0000000000e1', NULL, \
          '00000000-0000-0000-0000-00000000000a', '/videos/1.mkv', 1, 1, 'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f2', '00000000-0000-0000-0000-0000000000e2', NULL, \
          '00000000-0000-0000-0000-00000000000a', '/videos/2.mkv', 1, 2, 'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f3', '00000000-0000-0000-0000-0000000000e3', NULL, \
          '00000000-0000-0000-0000-00000000000a', '/videos/3.mkv', 1, 3, 'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f4', NULL, '00000000-0000-0000-0000-0000000000d3', \
          '00000000-0000-0000-0000-00000000000a', '/videos/4.mkv', 1, 4, 'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f5', NULL, NULL, \
          '00000000-0000-0000-0000-00000000000a', '/videos/5.mkv', 1, 5, 'unknown', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f7', NULL, '00000000-0000-0000-0000-0000000000d4', \
          '00000000-0000-0000-0000-00000000000a', '/videos/7.mkv', 1, 7, 'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f8', NULL, '00000000-0000-0000-0000-0000000000d5', \
          '00000000-0000-0000-0000-00000000000a', '/videos/8.mkv', 1, 8, 'known', now(), now())",
        "INSERT INTO files (id, episode_id, last_episode_number, library_id, file_path, \
                            file_size, hash_xxh3, file_status, scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f6', '00000000-0000-0000-0000-0000000000d3', 3, \
          '00000000-0000-0000-0000-00000000000a', '/videos/6.mkv', 1, 6, 'known', now(), now())",
        "UPDATE files SET missing_since = now() \
          WHERE id = '00000000-0000-0000-0000-0000000000f3'",
        // User 1: two sources of b1, the newer in progress; b2 with a
        // negative position and a zero duration; the episode finished; the
        // titleless file. User 2: b1 finished on one source, then started
        // on the other; b2 at NaN; episode 2 started on its own file, then
        // the episode 1-3 file finished, then episode 3 started on its own.
        // User 3: both sources of b1 at the same instant; b2 past its end.
        "INSERT INTO playback_progress (id, user_id, file_id, position_secs, duration_secs, \
                                        completed, updated_at) VALUES \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000f1', 100, 7200, false, '2026-01-01T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000f2', 300, 7200, false, '2026-01-02T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000f3', -5, 0, false, '2026-01-03T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000f4', 1150, 1200, true, '2026-01-04T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
          '00000000-0000-0000-0000-0000000000f5', 10, 100, false, '2026-01-05T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f1', 7000, 7200, true, '2026-01-01T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f2', 20, 7200, false, '2026-01-06T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f3', 'NaN', 'Infinity', false, \
          '2026-01-07T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f7', 50, 1200, false, '2026-01-02T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f6', 3590, 3600, true, '2026-01-08T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
          '00000000-0000-0000-0000-0000000000f8', 30, 1200, false, '2026-01-09T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c3', \
          '00000000-0000-0000-0000-0000000000f1', 10, 7200, false, '2026-01-03T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c3', \
          '00000000-0000-0000-0000-0000000000f2', 20, 7200, false, '2026-01-03T00:00:00Z'), \
         (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c3', \
          '00000000-0000-0000-0000-0000000000f3', 500, 100, false, '2026-01-04T00:00:00Z')",
    ];
    for sql in seed {
        db.execute_unprepared(sql).await.expect("seed legacy rows");
    }

    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("the migration applies over existing progress");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text(
            "SELECT concat_ws(' ', right(user_id::text, 2), \
                              right(coalesce(movie_id, episode_id)::text, 2), \
                              coalesce(right(show_id::text, 2), '-'), \
                              coalesce(right(last_file_id::text, 2), '-'), \
                              position_secs::text, \
                              coalesce(duration_secs::text, '-'), completed::text, \
                              play_count::text, \
                              to_char(last_played_at AT TIME ZONE 'UTC', 'MM-DD'), \
                              coalesce(dismissed_at::text, '-')) AS v \
               FROM watch_state ORDER BY 1"
        )
        .await,
        vec![
            // Newest source wins the place and the file.
            "c1 b1 - f2 300 7200 false 0 01-02 -",
            // A negative position is the start; a zero duration is unknown.
            "c1 b2 - f3 0 - false 0 01-03 -",
            // An episode keys by itself and carries its show; finished is
            // played, at the start, one play.
            "c1 d3 d1 f4 0 1200 true 1 01-04 -",
            // Finished on one source, restarted on another: played, and
            // resuming the restart.
            "c2 b1 - f2 20 7200 true 1 01-06 -",
            // NaN and Infinity are no position and no duration.
            "c2 b2 - f3 0 - false 0 01-07 -",
            // The run's file keys by its first episode...
            "c2 d3 d1 f6 0 3600 true 1 01-08 -",
            // ...and plays the rest of the run: an episode started before
            // goes back to the start, played...
            "c2 d4 d1 f7 0 1200 true 1 01-08 -",
            // ...and one started since keeps its place, played.
            "c2 d5 d1 f8 30 1200 true 1 01-09 -",
            // A tie goes to the larger file id.
            "c3 b1 - f2 20 7200 false 0 01-03 -",
            // A position past the duration is the duration; the file is
            // missing, and the row is kept.
            "c3 b2 - f3 100 100 false 0 01-04 -",
        ],
        "one row per user and title; the titleless file's row is dropped"
    );
    assert!(
        text(
            "SELECT table_name::text AS v FROM information_schema.tables \
              WHERE table_schema = current_schema() AND table_name = 'playback_progress'"
        )
        .await
        .is_empty(),
        "playback_progress is gone"
    );
    for (sql, why) in [
        (
            "INSERT INTO watch_state (id, user_id, movie_id, episode_id, show_id, \
                                      last_played_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
              '00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-0000000000d3', \
              '00000000-0000-0000-0000-0000000000d1', now())",
            "a row names one title",
        ),
        (
            "INSERT INTO watch_state (id, user_id, episode_id, last_played_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c2', \
              '00000000-0000-0000-0000-0000000000d3', now())",
            "an episode row carries its show",
        ),
        (
            "INSERT INTO watch_state (id, user_id, movie_id, last_played_at) VALUES \
             (gen_random_uuid(), '00000000-0000-0000-0000-0000000000c1', \
              '00000000-0000-0000-0000-0000000000b1', now())",
            "one row per user and movie",
        ),
        (
            "UPDATE watch_state SET position_secs = -1",
            "a position is never before the start",
        ),
        (
            "UPDATE watch_state SET duration_secs = 'NaN'",
            "a duration is a positive number",
        ),
    ] {
        assert!(db.execute_unprepared(sql).await.is_err(), "{why}");
    }

    // Purging a file keeps the state, without the file; deleting an episode
    // takes its state with it.
    db.execute_unprepared("DELETE FROM files WHERE id = '00000000-0000-0000-0000-0000000000f3'")
        .await
        .expect("purge a file");
    assert_eq!(
        text(
            "SELECT count(*)::text AS v FROM watch_state \
              WHERE movie_id = '00000000-0000-0000-0000-0000000000b2' AND last_file_id IS NULL"
        )
        .await,
        vec!["3"]
    );
    db.execute_unprepared(
        "DELETE FROM files WHERE id IN ('00000000-0000-0000-0000-0000000000f4', \
                                        '00000000-0000-0000-0000-0000000000f6'); \
         DELETE FROM episodes WHERE id = '00000000-0000-0000-0000-0000000000d3'",
    )
    .await
    .expect("delete an episode");
    assert_eq!(
        text(
            "SELECT count(*)::text AS v FROM watch_state \
              WHERE episode_id = '00000000-0000-0000-0000-0000000000d3'"
        )
        .await,
        vec!["0"]
    );

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert_eq!(
        text(
            "SELECT concat_ws(' ', right(user_id::text, 2), right(file_id::text, 2), \
                              position_secs::text, completed::text) AS v \
               FROM playback_progress ORDER BY 1"
        )
        .await,
        vec![
            "c1 f2 300 false",
            "c2 f2 20 true",
            "c2 f7 0 true",
            "c2 f8 30 true",
            "c3 f2 20 false",
        ],
        "each row goes back to the file it last played; one whose file is gone cannot"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");
    assert_eq!(
        text("SELECT count(*)::text AS v FROM watch_state").await,
        vec!["5"]
    );

    scoped.drop_schema().await.expect("drop schema");
}

/// Issue #233's migration: an existing file gains no part, a part sits only
/// on a movie file and is never below 1, and `down()` takes the column away
/// so `up()` can apply again.
#[tokio::test]
async fn the_movie_parts_migration_constrains_and_reverses() {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

    let scoped = ScopedSchema::create("movie_parts")
        .await
        .expect("create schema");
    let db = scoped.db();
    let db = db.as_ref();

    let before_this_one = beam_migration::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m20261012_000001_movie_parts")
        .expect("the migration is registered");
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(before_this_one as u32))
        .await
        .expect("every migration before this one applies");
    for sql in [
        "INSERT INTO libraries (id, name, root_path, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000a', 'lib', '/videos', now(), now())",
        "INSERT INTO movies (id, title, identity_key, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie - CD1', 'movie cd1|2019', now(), now())",
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, created_at) VALUES \
         ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, now())",
        "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                            file_status, scanned_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-000000000001', \
          '00000000-0000-0000-0000-00000000000a', '/videos/Movie (2019) - CD1.avi', 1, 1, \
          'known', now(), now()), \
         ('00000000-0000-0000-0000-0000000000f2', NULL, \
          '00000000-0000-0000-0000-00000000000a', '/videos/unknown.mkv', 1, 2, \
          'unknown', now(), now())",
    ] {
        db.execute_unprepared(sql).await.expect("seed rows");
    }
    up_all_or_nothing::<beam_migration::Migrator, _>(db, Some(1))
        .await
        .expect("this migration applies over existing rows");

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    assert_eq!(
        text("SELECT coalesce(part_number::text, 'none') AS v FROM files ORDER BY id").await,
        vec!["none", "none"],
        "an existing file has no part until it is reclassified"
    );
    db.execute_unprepared(
        "UPDATE files SET part_number = 1 WHERE id = '00000000-0000-0000-0000-0000000000f1'",
    )
    .await
    .expect("a movie file takes a part");
    for (sql, why) in [
        (
            "UPDATE files SET part_number = 0 WHERE id = '00000000-0000-0000-0000-0000000000f1'",
            "no part zero",
        ),
        (
            "UPDATE files SET part_number = 1 WHERE id = '00000000-0000-0000-0000-0000000000f2'",
            "a file that is no movie's has no part",
        ),
    ] {
        assert!(db.execute_unprepared(sql).await.is_err(), "{why}");
    }

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the migration rolls back");
    assert!(
        text(
            "SELECT column_name::text AS v FROM information_schema.columns \
              WHERE table_schema = current_schema() AND table_name = 'files' \
                AND column_name = 'part_number'"
        )
        .await
        .is_empty(),
        "down() drops the column"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}
