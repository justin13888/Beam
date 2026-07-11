use salvo::http::StatusCode;
use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::Serialize;

use crate::state::AppState;

/// Per-dependency check results reported by [`HealthStatus`].
#[derive(Serialize, ToSchema)]
pub struct HealthChecks {
    /// `"ok"` when the database round-trips, otherwise `"error: <reason>"`.
    pub database: String,
}

#[derive(Serialize, ToSchema)]
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

/// Health check endpoint.
///
/// Probes the server's external dependencies (currently just the database)
/// rather than reporting a static liveness value: a healthy result is `200`,
/// while any failing dependency yields `503 Service Unavailable` with a
/// `"degraded"` status so orchestrator health checks and monitors react to a
/// dependency outage.
#[endpoint(
    tags("health"),
    responses(
        (status_code = 200, description = "All dependencies are healthy", body = HealthStatus),
        (status_code = 503, description = "A dependency is unhealthy", body = HealthStatus),
    )
)]
#[tracing::instrument(skip_all)]
pub async fn health_check(depot: &mut Depot, res: &mut Response) {
    // The live router always injects `AppState`; a miss can only happen in a
    // stateless context (never served) and is treated as degraded.
    let (status, database, uptime_secs, code) = match depot.obtain::<AppState>() {
        Ok(state) => {
            let uptime_secs = state.uptime_secs();
            match state.probe.check_database().await {
                Ok(()) => ("healthy", "ok".to_string(), uptime_secs, StatusCode::OK),
                Err(e) => (
                    "degraded",
                    format!("error: {e}"),
                    uptime_secs,
                    StatusCode::SERVICE_UNAVAILABLE,
                ),
            }
        }
        Err(_) => (
            "degraded",
            "error: server state unavailable".to_string(),
            0,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    };

    res.status_code(code);
    res.render(Json(HealthStatus {
        status: status.to_string(),
        checks: HealthChecks { database },
        timestamp: chrono::Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs,
    }));
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
