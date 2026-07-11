//! Dependency health probing behind a trait seam so `/v1/health` can report a
//! deep, dependency-aware status without coupling the handler to a concrete
//! database connection.
//!
//! The production [`DbProbe`] pings the live Postgres pool; the
//! [`InMemoryDependencyProbe`] lets the zero-dependency test suite drive both
//! the healthy and the failing paths without a real database (see
//! `docs/testing.md`).

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
    db: DatabaseConnection,
}

impl DbProbe {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl DependencyProbe for DbProbe {
    async fn check_database(&self) -> Result<(), String> {
        self.db.ping().await.map_err(|e| e.to_string())
    }
}

/// Test double: reports a fixed (but at-runtime settable) database health,
/// so the failing branch of `/v1/health` is exercised without a real
/// connection drop.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
pub struct InMemoryDependencyProbe {
    database_healthy: std::sync::atomic::AtomicBool,
    error_message: String,
}

#[cfg(any(test, feature = "test-utils"))]
impl InMemoryDependencyProbe {
    /// A probe whose database check always succeeds.
    pub fn healthy() -> Self {
        Self {
            database_healthy: std::sync::atomic::AtomicBool::new(true),
            error_message: "database unreachable".to_string(),
        }
    }

    /// A probe whose database check always fails with `message`.
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            database_healthy: std::sync::atomic::AtomicBool::new(false),
            error_message: message.into(),
        }
    }

    /// Flip the reported database health for a running probe.
    pub fn set_database_healthy(&self, healthy: bool) {
        self.database_healthy
            .store(healthy, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl DependencyProbe for InMemoryDependencyProbe {
    async fn check_database(&self) -> Result<(), String> {
        if self
            .database_healthy
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            Ok(())
        } else {
            Err(self.error_message.clone())
        }
    }
}
