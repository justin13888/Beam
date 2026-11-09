//! Dependency health probing behind a trait seam so `/v1/health` can report a
//! deep, dependency-aware status without coupling the handler to a concrete
//! database connection.
//!
//! The production [`DbProbe`] pings the live Postgres pool; the
//! [`InMemoryDependencyProbe`] lets the zero-dependency test suite drive both
//! the healthy and the failing paths without a real database (see
//! `docs/testing.md`).

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;

/// Abstracts the liveness checks `/v1/health` performs against the server's
/// external dependencies. Kept deliberately narrow -- one method per probed
/// dependency -- so a later dependency (e.g. an object store) is an additive
/// method, not a reshape.
#[async_trait]
pub trait DependencyProbe: Send + Sync + std::fmt::Debug {
    /// Confirm the database is reachable. `Ok(())` when a round-trip
    /// succeeds; `Err(msg)` with a short, log-safe reason otherwise.
    async fn check_database(&self) -> Result<(), String>;
}

/// Production probe: round-trips the live Postgres pool via sea-orm's `ping`.
#[derive(Debug, Clone)]
pub struct DbProbe {
    db: Arc<DatabaseConnection>,
}

impl DbProbe {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl DependencyProbe for DbProbe {
    async fn check_database(&self) -> Result<(), String> {
        self.db.ping().await.map_err(|e| e.to_string())
    }
}
/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// Test double: reports a fixed database health, so the failing branch of
    /// `/v1/health` is exercised without a real connection drop.
    #[derive(Debug)]
    pub struct InMemoryDependencyProbe {
        /// `None` when the database check succeeds, else the error it reports.
        failure: Option<String>,
    }

    impl InMemoryDependencyProbe {
        /// A probe whose database check always succeeds.
        pub fn healthy() -> Self {
            Self { failure: None }
        }

        /// A probe whose database check always fails with `message`.
        pub fn failing(message: impl Into<String>) -> Self {
            Self {
                failure: Some(message.into()),
            }
        }
    }

    #[async_trait]
    impl DependencyProbe for InMemoryDependencyProbe {
        async fn check_database(&self) -> Result<(), String> {
            match &self.failure {
                None => Ok(()),
                Some(message) => Err(message.clone()),
            }
        }
    }
}

// Re-exported at the module root so the doubles keep the paths they had before
// they moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryDependencyProbe;
