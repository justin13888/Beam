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
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) VALUES \
         ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, true, now() - interval '2 days'), \
         ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, true, now() - interval '1 day'), \
         ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', 'Director''s Cut', true, now())",
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
            "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) \
             VALUES (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                     '00000000-0000-0000-0000-00000000000b', NULL, false, now())",
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
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) \
         VALUES (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, true, now()), \
                (gen_random_uuid(), '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, true, now())",
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
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) \
         VALUES ('00000000-0000-0000-0000-00000000000e', '00000000-0000-0000-0000-00000000000a', \
                 '00000000-0000-0000-0000-00000000000b', NULL, true, now())",
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

    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
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
    let steps_back =
        <u32 as TryFrom<usize>>::try_from(migrations.len() - this_one).expect("few migrations");
    beam_migration::Migrator::down(db, Some(steps_back))
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
        "INSERT INTO movie_entries (id, library_id, movie_id, edition, is_primary, created_at) VALUES \
         ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000000a', \
          '00000000-0000-0000-0000-00000000000b', NULL, true, now())",
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
                AND indexname = 'idx_metadata_enrichment_list'"
        )
        .await
        .is_empty(),
        "down() drops the column and the index"
    );
    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("the migration reapplies over the rolled-back schema");

    scoped.drop_schema().await.expect("drop schema");
}
