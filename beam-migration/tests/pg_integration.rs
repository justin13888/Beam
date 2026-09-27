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

use beam_migration::up_all_or_nothing;
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
