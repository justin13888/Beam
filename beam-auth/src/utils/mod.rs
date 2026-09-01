pub mod admin_claim;
// Shared behavioural contracts. Not `cfg`-gated: the module holds only
// `macro_rules!` definitions, which are inert until invoked, and a
// `#[macro_export]` inside a `cfg`-gated module cannot be referred to by an
// absolute path from within this crate.
pub mod contract;
pub mod hex;
pub mod models;
pub mod oidc;
pub mod oidc_config;
pub mod pending_auth_store;
pub mod repository;
pub mod session_store;

#[cfg(test)]
#[path = "sql_shape_tests.rs"]
mod sql_shape_tests;
