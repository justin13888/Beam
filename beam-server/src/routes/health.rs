use chrono::{DateTime, Utc};
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::routes::tags::Health;
use crate::state::AppState;

/// The server's overall health: `healthy` exactly when every dependency
/// check is `ok`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Degraded,
}

/// The outcome of probing one dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Error,
}

/// One probed dependency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct DependencyCheck {
    pub status: CheckStatus,
    /// Why the check failed; absent when it passed.
    pub detail: Option<String>,
}

impl DependencyCheck {
    fn from_probe<E: std::fmt::Display>(result: Result<(), E>) -> Self {
        match result {
            Ok(()) => Self {
                status: CheckStatus::Ok,
                detail: None,
            },
            Err(error) => Self {
                status: CheckStatus::Error,
                detail: Some(error.to_string()),
            },
        }
    }
}

/// Per-dependency check results reported by [`HealthStatus`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct HealthChecks {
    /// Whether the database round-trips.
    pub database: DependencyCheck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct HealthStatus {
    /// `healthy` when every dependency check passed, `degraded` otherwise.
    pub status: HealthState,
    /// Result of each probed dependency.
    pub checks: HealthChecks,
    /// When the checks ran.
    pub checked_at: DateTime<Utc>,
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

    let checked_at = state.clock().now();
    let checks = HealthChecks {
        database: DependencyCheck::from_probe(state.probe.check_database().await),
    };
    // Destructured so a new dependency cannot be added without deciding here
    // whether it degrades the server.
    let HealthChecks { database } = &checks;
    let status = if database.status == CheckStatus::Ok {
        HealthState::Healthy
    } else {
        HealthState::Degraded
    };

    let body = HealthStatus {
        status,
        checks,
        checked_at,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_secs,
    };

    match status {
        HealthState::Healthy => HealthReply::Healthy(body),
        HealthState::Degraded => HealthReply::Degraded(body),
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
