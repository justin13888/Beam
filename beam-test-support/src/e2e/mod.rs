//! Harness for the opt-in `e2e-server` tier (#191).
//!
//! Every other Rust tier stops at a seam: the hermetic suite at the trait
//! line, the `pg-integration` tier at the database. This one runs the
//! composed stack -- a real Postgres, a real Dex and the `beam-server` binary
//! itself -- over a real library on disk, and drives it the way a native
//! client does: through the generated wire client in `beam-client-core`.
//!
//! What it needs, and where it reads each from:
//!
//! * **Postgres** -- [`database::DATABASE_URL_ENV`]. Each test gets a database
//!   of its own, created fresh and dropped afterwards.
//! * **Dex** -- [`idp::ISSUER_ENV`], defaulting to the issuer `dex/config.yaml`
//!   declares. Its static users are the accounts the suite signs in as.
//! * **An FFmpeg CLI** with libx264 and AAC on `PATH` -- the fixture media is
//!   generated per run ([`fixtures`]), never committed.
//!
//! `mise run e2e:server` composes the first two and runs the tier. Nothing
//! here is ever part of `cargo test --workspace` (NFR-201): the whole module
//! compiles only under the `e2e-server` feature.
//!
//! The source media is the one thing the server must never change (FR-202),
//! so the library tree is a [`library::FixtureLibrary`]: every byte, link and
//! directory in it is recorded as the test writes it, the tree is sealed
//! read-only while the server runs, and it is checked against that record
//! afterwards.

pub mod client;
pub mod database;
pub mod fixtures;
pub mod idp;
pub mod library;
pub mod server;
pub mod stack;
pub mod wait;

pub use stack::{Stack, StackOptions};
