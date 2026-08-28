//! The opt-in `pg-integration` tier.
//!
//! Compiled to nothing unless `--features pg-integration` is passed, so the
//! default `cargo test --workspace` still needs no infrastructure (NFR-201).
//! Run it with:
//!
//! ```text
//! docker compose -f compose.dependencies.yaml up -d
//! mise run rust:test:pg
//! ```
//!
//! What belongs here is only what a real Postgres can tell us and the hermetic
//! tier cannot: that the shared behavioural contracts hold against the actual
//! SQL, that `ON CONFLICT` is atomic, that the foreign keys and unique indexes
//! the migrations declare are really there, and that the migrations reverse.
#![cfg(feature = "pg-integration")]

#[path = "pg_integration/file_repository.rs"]
mod file_repository;
#[path = "pg_integration/playback_progress.rs"]
mod playback_progress;
#[path = "pg_integration/schema.rs"]
mod schema;
