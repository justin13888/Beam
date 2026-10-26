//! The opt-in `e2e-server` tier (#191): the server end to end, over the
//! composed stack and a real library that changes under it.
//!
//! Compiled to nothing unless `--features e2e-server` is passed, so the
//! default `cargo test --workspace` still needs no infrastructure (NFR-201).
//! Run it with:
//!
//! ```text
//! mise run e2e:server
//! ```
//!
//! which composes Postgres and Dex and runs this binary against them. Each
//! test starts its own `beam-server` process -- the binary this package
//! builds -- over a database and a library of its own, signs in through Dex,
//! and drives the frozen `/v1` contract with the generated client the native
//! apps use. Every test ends by proving the library tree is byte-for-byte what
//! the test made it (FR-202): the server reads the source media and writes
//! nothing there.
//!
//! The harness lives in `beam-test-support`'s `e2e` module; see
//! `docs/testing.md` for what this tier is for and what it leaves out.
#![cfg(feature = "e2e-server")]

#[path = "e2e_server/common.rs"]
mod common;

#[path = "e2e_server/admin.rs"]
mod admin;
#[path = "e2e_server/catalog.rs"]
mod catalog;
#[path = "e2e_server/metadata.rs"]
mod metadata;
#[path = "e2e_server/playback.rs"]
mod playback;
#[path = "e2e_server/reconciliation.rs"]
mod reconciliation;
#[path = "e2e_server/scan.rs"]
mod scan;
#[path = "e2e_server/sign_in.rs"]
mod sign_in;
