use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::routes::tags::Health;
use crate::state::AppState;

/// Per-dependency check results reported by [`HealthStatus`].
#[derive(Serialize, Deserialize, Schema)]
pub struct HealthChecks {
    /// `"ok"` when the database round-trips, otherwise `"error: <reason>"`.
    pub database: String,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct HealthStatus {
    /// `"healthy"` when every dependency check passed, `"degraded"` otherwise.
    pub status: String,
    /// Result of each probed dependency.
    pub checks: HealthChecks,
    pub timestamp: String,
    pub version: String,
    /// Whole seconds the process has been serving.
    pub uptime_secs: u64,
}

/// The two answers a health probe can give.
///
/// An enum rather than a runtime status code: `Reply` keys its variants by
/// status, so "degraded" and 503 are the same fact stated once. Both variants
/// carry the same body because a monitor reading a 503 still wants to know
/// *which* dependency failed.
#[derive(Reply)]
pub enum HealthReply {
    #[reply(status = 200, description = "All dependencies are healthy")]
    Healthy(HealthStatus),

    #[reply(status = 503, description = "A dependency is unhealthy")]
    Degraded(HealthStatus),
}

/// Health check endpoint.
///
/// Probes the server's external dependencies (currently just the database)
/// rather than reporting a static liveness value: a healthy result is `200`,
/// while any failing dependency yields `503 Service Unavailable` with a
/// `"degraded"` status so orchestrator health checks and monitors react to a
/// dependency outage.
///
/// Note what is gone relative to the Salvo implementation: there is no
/// "server state unavailable" arm. `Inject<AppState>` cannot fail, so the
/// third branch the old handler needed -- a depot miss it reported as
/// degraded -- is not a state this can reach.
#[kynos::get("/health", tag = Health, operation_id = "getHealth")]
#[tracing::instrument(skip_all)]
pub async fn health_check(Inject(state): Inject<AppState>) -> HealthReply {
    let uptime_secs = state.uptime_secs();

    let (status, database, healthy) = match state.probe.check_database().await {
        Ok(()) => ("healthy", "ok".to_owned(), true),
        Err(e) => ("degraded", format!("error: {e}"), false),
    };

    let body = HealthStatus {
        status: status.to_owned(),
        checks: HealthChecks { database },
        timestamp: chrono::Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_secs,
    };

    if healthy {
        HealthReply::Healthy(body)
    } else {
        HealthReply::Degraded(body)
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
