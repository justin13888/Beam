//! Zero-dependency tests for the Prometheus wiring: the `/metrics` route's
//! mount-only-when-enabled behavior, the request-metrics hoop, and the
//! low-cardinality route classifier.
//!
//! Recorder discipline: no test installs a *global* recorder (parallel tests
//! would collide). The endpoint tests use `PrometheusBuilder::build_recorder`
//! (a handle without global installation); the middleware tests use
//! `metrics::with_local_recorder` with a `metrics_util` `DebuggingRecorder`,
//! driving the async service via a current-thread runtime inside the local-
//! recorder scope so every recorded sample lands in the snapshot.

use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use salvo::http::Method;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};

use super::{HttpMetrics, classify_route};
use crate::routes::rate_limit::{RateLimiter, RealClock};
use crate::routes::{create_router, test_support};

#[handler]
async fn ok_handler() -> &'static str {
    "ok"
}

// ─── classify_route ─────────────────────────────────────────────────────────

#[test]
fn classify_route_covers_every_class_and_falls_back_to_other() {
    let get = Method::GET;
    let cases = [
        ("/v1/health", "health"),
        ("/v1/media", "media_browse"),
        (
            "/v1/media/22222222-2222-2222-2222-222222222222",
            "media_detail",
        ),
        (
            "/v1/media/22222222-2222-2222-2222-222222222222/sources",
            "media_sources",
        ),
        ("/v1/genres", "genres"),
        ("/v1/files/abc/stream", "stream"),
        ("/v1/files/abc/download", "download"),
        ("/v1/files/abc/progress", "progress"),
        ("/v1/continue-watching", "continue_watching"),
        ("/v1/history", "history"),
        ("/v1/libraries", "libraries"),
        ("/v1/libraries/abc", "libraries"),
        ("/v1/libraries/abc/files", "libraries"),
        ("/v1/admin/libraries", "admin"),
        ("/v1/admin/logs/count", "admin"),
        ("/v1/auth/login", "auth"),
        ("/v1/auth/callback", "auth"),
        ("/v1/me", "session"),
        ("/v1/logout", "session"),
        ("/v1/logout-all", "session"),
        ("/v1/sessions", "session"),
        ("/v1/sessions/abc", "session"),
        // Fallbacks: unknown subpaths and non-v1 prefixes must never emit a
        // raw (unbounded) path.
        ("/v1/unknown", "other"),
        ("/v1/media/x/y/z", "other"),
        ("/v2/health", "other"),
        ("/metrics", "other"),
        ("/", "other"),
    ];

    for (path, expected) in cases {
        assert_eq!(
            classify_route(&get, path),
            expected,
            "path {path} misclassified"
        );
    }
}

// ─── /metrics endpoint mounting ─────────────────────────────────────────────

#[tokio::test]
async fn metrics_endpoint_renders_prometheus_text_when_enabled() {
    let recorder = PrometheusBuilder::new().build_recorder();
    // Record through the *local* recorder (never installed globally) so the
    // rendered exposition provably comes from the handle we mounted.
    metrics::with_local_recorder(&recorder, || {
        metrics::counter!("beam_test_events_total").increment(3);
    });

    let router = create_router(test_support::make_app_state(), Some(recorder.handle()));
    let service = Service::new(router);

    let mut res = TestClient::get("http://localhost/metrics")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_string().await.unwrap();
    assert!(
        body.contains("beam_test_events_total 3"),
        "exposition should contain the recorded counter, got: {body}"
    );
}

#[tokio::test]
async fn metrics_endpoint_is_absent_when_disabled() {
    let router = create_router(test_support::make_app_state(), None);
    let service = Service::new(router);

    let res = TestClient::get("http://localhost/metrics")
        .send(&service)
        .await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::NOT_FOUND),
        "GET /metrics must 404 when BEAM_ENABLE_METRICS is off"
    );
}

#[tokio::test]
async fn metrics_route_never_appears_in_openapi_spec() {
    // The live router merges its own OpenAPI doc in main; /metrics is a plain
    // Handler precisely so that merge never picks it up.
    let recorder = PrometheusBuilder::new().build_recorder();
    let router = create_router(test_support::make_app_state(), Some(recorder.handle()));
    let doc = salvo::oapi::OpenApi::new("test", "0.0.0").merge_router(&router);
    let spec = serde_json::to_value(&doc).unwrap();
    let paths = spec["paths"].as_object().unwrap();

    assert!(
        paths.keys().all(|p| !p.contains("metrics")),
        "spec must not mention /metrics, got paths: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(
        paths.contains_key("/v1/health"),
        "sanity: real endpoints still present"
    );
}

// ─── request-metrics middleware ─────────────────────────────────────────────

/// Snapshot lookup: the value of `beam_http_requests_total` for an exact
/// (method, route, status) label set, if recorded.
fn counter_value(
    snapshot: &[(
        metrics_util::CompositeKey,
        Option<metrics::Unit>,
        Option<metrics::SharedString>,
        DebugValue,
    )],
    method: &str,
    route: &str,
    status: &str,
) -> Option<u64> {
    snapshot.iter().find_map(|(key, _, _, value)| {
        let key = key.key();
        if key.name() != "beam_http_requests_total" {
            return None;
        }
        let mut labels: Vec<(&str, &str)> = key.labels().map(|l| (l.key(), l.value())).collect();
        labels.sort_unstable();
        let mut expected = vec![("method", method), ("route", route), ("status", status)];
        expected.sort_unstable();
        if labels != expected {
            return None;
        }
        match value {
            DebugValue::Counter(v) => Some(*v),
            _ => None,
        }
    })
}

/// Whether the duration histogram was recorded for (method, route).
fn histogram_recorded(
    snapshot: &[(
        metrics_util::CompositeKey,
        Option<metrics::Unit>,
        Option<metrics::SharedString>,
        DebugValue,
    )],
    method: &str,
    route: &str,
) -> bool {
    snapshot.iter().any(|(key, _, _, value)| {
        let key = key.key();
        key.name() == "beam_http_request_duration_seconds"
            && key
                .labels()
                .any(|l| l.key() == "method" && l.value() == method)
            && key
                .labels()
                .any(|l| l.key() == "route" && l.value() == route)
            && matches!(value, DebugValue::Histogram(samples) if !samples.is_empty())
    })
}

#[test]
fn middleware_records_success_and_rate_limited_requests() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // The local recorder guard is thread-scoped; a current-thread runtime
    // keeps every handler (and thus every metrics call) on this thread.
    metrics::with_local_recorder(&recorder, || {
        rt.block_on(async {
            // HttpMetrics outermost (first hoop), exactly as create_router
            // mounts it; the rate limiter (capacity 1) sits inside it so its
            // 429 is observed by the metrics hoop.
            let limiter = RateLimiter::new(1, false, Arc::new(RealClock));
            let router = Router::with_path("v1").hoop(HttpMetrics).push(
                Router::new()
                    .push(Router::with_path("health").get(ok_handler))
                    .push(
                        Router::with_path("auth/login")
                            .hoop(limiter)
                            .get(ok_handler),
                    ),
            );
            let service = Service::new(router);

            // Two successful health checks.
            for _ in 0..2 {
                let res = TestClient::get("http://localhost/v1/health")
                    .send(&service)
                    .await;
                assert_eq!(res.status_code, Some(StatusCode::OK));
            }

            // One allowed auth request, then one rate-limited 429.
            let res = TestClient::get("http://localhost/v1/auth/login")
                .send(&service)
                .await;
            assert_eq!(res.status_code, Some(StatusCode::OK));
            let res = TestClient::get("http://localhost/v1/auth/login")
                .send(&service)
                .await;
            assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
        })
    });

    let snapshot = snapshotter.snapshot().into_vec();

    assert_eq!(
        counter_value(&snapshot, "GET", "health", "200"),
        Some(2),
        "both health requests must be counted; snapshot: {snapshot:?}"
    );
    assert_eq!(
        counter_value(&snapshot, "GET", "auth", "200"),
        Some(1),
        "the allowed auth request must be counted"
    );
    assert_eq!(
        counter_value(&snapshot, "GET", "auth", "429"),
        Some(1),
        "the rate-limited 429 must be counted (metrics hoop wraps the limiter)"
    );
    assert!(
        histogram_recorded(&snapshot, "GET", "health"),
        "request durations must be recorded"
    );
}
