//! Harness for the opt-in `pg-integration` tier.
//!
//! Everything here is gated behind the `pg-integration` feature, which is off
//! by default: with the feature off this crate compiles to an empty library and
//! pulls in no dependencies, so `cargo test --workspace` still runs with zero
//! infrastructure (NFR-201).
//!
//! See `docs/testing.md` for what the tier is for -- only semantics a real
//! Postgres has (`ON CONFLICT` atomicity, foreign keys, `pg_trgm`, index usage,
//! migration up/down) -- and `ADR-0011` for why it exists at all.

#[cfg(feature = "pg-integration")]
pub mod postgres;

#[cfg(feature = "pg-integration")]
pub mod seed;
