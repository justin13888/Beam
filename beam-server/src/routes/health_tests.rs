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

    use beam_domain::services::TestClock;
    use chrono::{DateTime, Utc};

    use crate::routes::health::health_check;
    use crate::routes::test_support::make_app_state_with_probe;
    use crate::services::health::{DependencyProbe, InMemoryDependencyProbe};
    use crate::state::AppState;

    /// The instant every probe here runs at, so `checked_at` is asserted
    /// exactly rather than as "some string".
    fn checked_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-03-01T12:00:00Z")
            .expect("a valid instant")
            .with_timezone(&Utc)
    }

    /// The health endpoint alone, over a state whose probe the caller chose,
    /// on a clock stopped at [`checked_at`].
    fn client(probe: Arc<dyn DependencyProbe>) -> TestClient<AppState> {
        let service = Router::new()
            .nest("/v1", Router::new().mount(kynos::routes![health_check]))
            .build(make_app_state_with_probe(
                probe,
                Arc::new(TestClock::starting_at(checked_at())),
            ))
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
        assert_eq!(body["checks"]["database"]["status"], "ok");
        assert!(
            body["checks"]["database"]
                .as_object()
                .is_some_and(|check| !check.contains_key("detail")),
            "a passing check has nothing to explain, so `detail` is absent, not null: {body}"
        );
        assert!(body["uptime_secs"].is_u64(), "uptime_secs must be present");
        assert!(body["version"].is_string());
        let reported: DateTime<Utc> = serde_json::from_value(body["checked_at"].clone())
            .expect("checked_at is an RFC 3339 date-time");
        assert_eq!(reported, checked_at(), "checked_at is read from the clock");
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
        assert_eq!(body["checks"]["database"]["status"], "error");
        assert_eq!(body["checks"]["database"]["detail"], "connection refused");
        assert!(body["uptime_secs"].is_u64());
    }
}
