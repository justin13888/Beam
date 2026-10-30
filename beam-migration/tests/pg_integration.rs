//! The opt-in `pg-integration` tier for beam-migration, against a real
//! Postgres: the upgrade path is all-or-nothing and safe beside a concurrent
//! migrator ([`upgrade_path`]), and each data-carrying migration transforms
//! what it finds and reverses ([`migrations`]).
//!
//! Compiled to nothing unless `--features pg-integration` is passed, so the
//! default `cargo test --workspace` still needs no infrastructure (NFR-201).
//! Run with `mise run rust:test:pg`.
//!
//! Each test runs in its own `ScopedSchema`, so the ledger and tables it
//! creates are invisible to every other test.
#![cfg(feature = "pg-integration")]

#[path = "pg_integration/migrations.rs"]
mod migrations;
#[path = "pg_integration/upgrade_path.rs"]
mod upgrade_path;
