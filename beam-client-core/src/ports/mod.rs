//! Boundaries the core depends on but does not implement.
//!
//! Every one of these is a trait with an in-memory fake alongside it, so the
//! core's own tests never need a network, a filesystem, or a running Android
//! runtime -- the same trait-plus-fake discipline `beam-server` applies to its
//! repositories.

pub mod byte_source;
// Shared behavioural contracts. Not `cfg`-gated: the module holds only
// `macro_rules!` definitions, which are inert until invoked, and a
// `#[macro_export]` inside a `cfg`-gated module cannot be referred to by an
// absolute path from within this crate.
pub mod contract;
pub mod kv;
