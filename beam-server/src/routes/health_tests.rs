//! Subcutaneous tests for the dependency-aware `/v1/health` endpoint.
//!
//! The endpoint touches only the injected [`DependencyProbe`] and
//! `AppState::uptime_secs`, so these drive the real handler through Kynos's
//! in-process `TestClient` over a state built by `test_support`. No Postgres,
//! no Docker, no listener.
//!
//! A failing dependency is reached by configuring the probe to return an error
//! (NFR-205), never by breaking a real one.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kynos::http::StatusCode;
    use kynos::prelude::*;
    use kynos::test::TestClient;
    use serde_json::Value;

    use crate::routes::health::health_check;
    use crate::routes::test_support::make_app_state_with_probe;
    use crate::services::health::{DependencyProbe, InMemoryDependencyProbe};
    use crate::state::AppState;

    /// The health endpoint alone, over a state whose probe the caller chose.
    fn client(probe: Arc<dyn DependencyProbe>) -> TestClient<AppState> {
        let service = Router::new()
            .nest("/v1", Router::new().mount(kynos::routes![health_check]))
            .build(make_app_state_with_probe(probe))
            .expect("the health router describes itself");

        TestClient::new(service)
    }

    #[tokio::test]
    async fn healthy_database_yields_200_with_ok_check_and_uptime() {
        let response = client(Arc::new(InMemoryDependencyProbe::healthy()))
            .get("/v1/health")
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body: Value = response.json();
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["checks"]["database"], "ok");
        assert!(body["uptime_secs"].is_u64(), "uptime_secs must be present");
        assert!(body["version"].is_string());
        assert!(body["timestamp"].is_string());
    }

    #[tokio::test]
    async fn failing_database_yields_503_degraded_with_error_surfaced() {
        let response = client(Arc::new(InMemoryDependencyProbe::failing(
            "connection refused",
        )))
        .get("/v1/health")
        .send()
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body: Value = response.json();
        assert_eq!(body["status"], "degraded");
        assert_eq!(body["checks"]["database"], "error: connection refused");
        assert!(body["uptime_secs"].is_u64());
    }
}
