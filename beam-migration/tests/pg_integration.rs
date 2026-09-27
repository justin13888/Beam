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
        "INSERT INTO movies (id, title, created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-00000000000b', 'Movie', now(), now())",
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

/// Issue #182's migration reverses: `down()` drops the two columns (and the
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

    up_all_or_nothing::<beam_migration::Migrator, _>(db, None)
        .await
        .expect("every migration applies");
    let last = beam_migration::Migrator::migrations()
        .last()
        .map(|m| m.name().to_string());
    assert_eq!(
        last.as_deref(),
        Some("m20260929_000001_classifier_v2"),
        "rolling back one step reverts exactly this migration"
    );

    let text = |sql: &'static str| async move {
        db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("query")
            .into_iter()
            .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
            .collect::<Vec<String>>()
    };
    let added_columns = "SELECT column_name::text AS v FROM information_schema.columns \
                          WHERE table_schema = current_schema() AND table_name = 'files' \
                            AND column_name IN ('classifier_version', 'last_episode_number') \
                          ORDER BY column_name";
    let nulls_not_distinct = "SELECT i.indnullsnotdistinct::text AS v FROM pg_index i \
                               JOIN pg_class c ON c.oid = i.indexrelid \
                               JOIN pg_namespace n ON n.oid = c.relnamespace \
                              WHERE c.relname = 'idx_movie_entries_unique' \
                                AND n.nspname = current_schema()";
    assert_eq!(
        text(added_columns).await,
        vec!["classifier_version", "last_episode_number"]
    );
    assert_eq!(text(nulls_not_distinct).await, vec!["true"]);

    beam_migration::Migrator::down(db, Some(1))
        .await
        .expect("the classifier migration rolls back");

    assert!(
        text(added_columns).await.is_empty(),
        "down() drops both columns"
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
