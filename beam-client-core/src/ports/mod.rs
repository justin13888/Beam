//! Boundaries the core depends on but does not implement.
//!
//! Every one of these is a trait with an in-memory fake alongside it, so the
//! core's own tests never need a network, a filesystem, or a running Android
//! runtime -- the same trait-plus-fake discipline `beam-server` applies to its
//! repositories.

pub mod byte_source;
pub mod kv;
