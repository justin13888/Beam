//! The opt-in `pg-integration` tier for beam-auth.
//!
//! Compiled to nothing unless `--features pg-integration` is passed, so the
//! default `cargo test --workspace` still needs no infrastructure (NFR-201).
//! Run with `mise run rust:test:pg`.
#![cfg(feature = "pg-integration")]

#[path = "pg_integration/pending_auth_store.rs"]
mod pending_auth_store;
#[path = "pg_integration/session_store.rs"]
mod session_store;
#[path = "pg_integration/user_repository.rs"]
mod user_repository;
