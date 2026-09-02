//! Zero-dependency tests for the Prometheus wiring: the `/metrics` operation,
//! and the observer that counts every response the router produces.
//!
//! Recorder discipline: no test installs a *global* recorder (parallel tests
//! would collide). The endpoint tests use `PrometheusBuilder::build_recorder`
//! (a handle without global installation); the observer tests use
//! `metrics::with_local_recorder` with a `metrics_util` `DebuggingRecorder`,
//! driving the async service via a current-thread runtime inside the local-
//! recorder scope so every recorded sample lands in the snapshot.
//!
//! `classify_route` had three unit tests here and they are gone with it. The
//! `route` label is now `Route::path()` -- the router's own `paths` key -- so
//! there is no hand-maintained table left to test, and writing the expected
//! labels out again would be the mirror AGENTS.md forbids. What replaces them
//! is an assertion that a request carrying an id is counted under the
//! *template*, which is the property the classifier existed to provide.

use std::collections::BTreeSet;
use std::sync::Arc;

use kynos::http::body::Body;
use kynos::http::{Request, StatusCode};
use kynos::prelude::*;
use kynos::router::service::Service;
use kynos::test::TestClient;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use serde_json::Value;

use super::UNMATCHED;
use crate::config::ServerConfig;
use crate::routes::create_router;
use crate::routes::test_support::make_app_state_full;
use crate::services::health::InMemoryDependencyProbe;
use crate::state::AppState;

/// The whole router, over a state carrying `metrics` and the configuration the
/// caller wrote.
///
/// `create_router` takes no arguments now: the Prometheus handle arrives on
/// `AppState`, which is what lets the served surface and the exported document
/// come from one walk of the same value.
fn service(
    metrics: Option<PrometheusHandle>,
    adjust: impl FnOnce(&mut ServerConfig),
) -> Service<AppState> {
    let state = make_app_state_full(
        adjust,
        Arc::new(beam_domain::services::RealClock),
        Arc::new(InMemoryDependencyProbe::healthy()),
        metrics,
    );

    create_router()
        .build(state)
        .expect("the server router describes itself")
}

// ─── the /metrics operation ─────────────────────────────────────────────────

#[tokio::test]
async fn metrics_endpoint_renders_prometheus_text_when_a_recorder_is_installed() {
    let recorder = PrometheusBuilder::new().build_recorder();
    // Record through the *local* recorder (never installed globally) so the
    // rendered exposition provably comes from the handle on the state.
    metrics::with_local_recorder(&recorder, || {
        metrics::counter!("beam_test_events_total").increment(3);
    });

    let client = TestClient::new(service(Some(recorder.handle()), |_| {}));
    let response = client.get("/metrics").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.text().contains("beam_test_events_total 3"),
        "exposition should contain the recorded counter, got: {}",
        response.text()
    );
}

/// The route is mounted in every build; only its answer depends on
/// configuration. Under Salvo this was a 404, because the route itself was
/// conditional -- which made the served surface differ from the document.
#[tokio::test]
async fn metrics_endpoint_reports_503_when_no_recorder_is_installed() {
    let client = TestClient::new(service(None, |_| {}));

    client
        .get("/metrics")
        .send()
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#metrics-unavailable");
}

/// The inverse of what Salvo's implementation asserted. `/metrics` used to be
/// a plain `Handler` the OpenAPI merge could not see, so it stayed out of the
/// document by accident of the framework; Kynos routes and describes from one
/// declaration, so its presence must be deliberate and marked instead.
#[test]
fn the_metrics_route_is_described_and_tagged_internal() {
    let service = service(None, |_| {});
    let document: Value =
        serde_json::to_value(service.openapi()).expect("the description serialises");

    let operation = &document["paths"]["/metrics"]["get"];
    assert!(
        operation.is_object(),
        "GET /metrics must appear in the exported document, got paths: {:?}",
        document["paths"]
            .as_object()
            .map(|paths| paths.keys().collect::<Vec<_>>())
    );
    assert_eq!(
        operation["tags"],
        serde_json::json!(["internal"]),
        "the scrape target must be marked as outside the client contract"
    );
}

// ─── the response observer ──────────────────────────────────────────────────

/// One entry of a `DebuggingRecorder` snapshot.
type Sample = (
    metrics_util::CompositeKey,
    Option<metrics::Unit>,
    Option<metrics::SharedString>,
    DebugValue,
);

/// Whether `key` carries exactly `labels`.
fn labelled(key: &metrics::Key, labels: &[(&str, &str)]) -> bool {
    let mut actual: Vec<(&str, &str)> = key.labels().map(|l| (l.key(), l.value())).collect();
    actual.sort_unstable();
    let mut expected = labels.to_vec();
    expected.sort_unstable();
    actual == expected
}

/// The counter recorded under `name` for exactly this label set, if any.
fn counter(snapshot: &[Sample], name: &str, labels: &[(&str, &str)]) -> Option<u64> {
    snapshot.iter().find_map(|(key, _, _, value)| {
        let key = key.key();
        match value {
            DebugValue::Counter(count) if key.name() == name && labelled(key, labels) => {
                Some(*count)
            }
            _ => None,
        }
    })
}

/// Whether a non-empty histogram was recorded under `name` for `labels`.
fn histogram_recorded(snapshot: &[Sample], name: &str, labels: &[(&str, &str)]) -> bool {
    snapshot.iter().any(|(key, _, _, value)| {
        let key = key.key();
        key.name() == name
            && labelled(key, labels)
            && matches!(value, DebugValue::Histogram(samples) if !samples.is_empty())
    })
}

/// Every distinct `route` label the snapshot carries.
fn route_labels(snapshot: &[Sample]) -> BTreeSet<String> {
    snapshot
        .iter()
        .flat_map(|(key, _, _, _)| {
            key.key()
                .labels()
                .filter(|label| label.key() == "route")
                .map(|label| label.value().to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A current-thread runtime, so every handler -- and thus every metrics call --
/// stays on the thread the local-recorder guard is scoped to.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

#[test]
fn responses_are_counted_under_the_path_template_rather_than_the_request_path() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let rt = runtime();

    // Read back rather than assumed: `/v1/media/{id}` is session-gated and
    // `/nope` matches nothing, and the point of the test is the *route* label,
    // not which status each of them happens to produce.
    let (health, detail, unmatched) = metrics::with_local_recorder(&recorder, || {
        rt.block_on(async {
            let client = TestClient::new(service(None, |_| {}));

            let health = client.get("/v1/health").send().await.status();
            let _second = client.get("/v1/health").send().await.status();
            let detail = client
                .get("/v1/media/0197c0de-dead-beef-cafe-000000000001")
                .send()
                .await
                .status();
            let unmatched = client.get("/nope").send().await.status();

            (health, detail, unmatched)
        })
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let status = |code: StatusCode| code.as_u16().to_string();

    assert_eq!(
        counter(
            &snapshot,
            "beam_http_requests_total",
            &[
                ("method", "GET"),
                ("route", "/v1/health"),
                ("status", &status(health)),
            ],
        ),
        Some(2),
        "both health requests must be counted; snapshot: {snapshot:?}"
    );
    assert!(
        histogram_recorded(
            &snapshot,
            "beam_http_request_duration_seconds",
            &[("method", "GET"), ("route", "/v1/health")],
        ),
        "request durations must be recorded"
    );

    // The whole point of the label: the id in the request path never reaches a
    // time series.
    assert_eq!(
        counter(
            &snapshot,
            "beam_http_requests_total",
            &[
                ("method", "GET"),
                ("route", "/v1/media/{id}"),
                ("status", &status(detail)),
            ],
        ),
        Some(1),
        "a parameterised route is counted under its template; snapshot: {snapshot:?}"
    );

    // A request that matched nothing is counted, under the one label a scanner
    // cannot vary. There is no operation, so the method is unknown too: the
    // pair comes from the same `Option<Route>`.
    assert_eq!(
        counter(
            &snapshot,
            "beam_http_requests_total",
            &[
                ("method", UNMATCHED),
                ("route", UNMATCHED),
                ("status", &status(unmatched)),
            ],
        ),
        Some(1),
        "an unmatched request is counted, not dropped; snapshot: {snapshot:?}"
    );

    let routes = route_labels(&snapshot);
    assert!(
        routes
            .iter()
            .all(|route| !route.contains("0197c0de-dead-beef-cafe-000000000001")),
        "no request path may reach a label; saw {routes:?}"
    );
    assert!(
        routes.iter().all(|route| route != "/nope"),
        "an invented path may not mint a time series; saw {routes:?}"
    );
}

/// The observer runs outside the interceptor chain, so a response no handler
/// produced is still counted. Under Salvo this depended on `HttpMetrics` being
/// installed as the outermost hoop; now it is structural.
#[test]
fn a_short_circuited_response_is_counted_even_though_no_handler_ran() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let rt = runtime();

    metrics::with_local_recorder(&recorder, || {
        rt.block_on(async {
            // One token of auth budget, and every request here shares the one
            // bucket a socketless client counts against.
            let client = TestClient::new(service(None, |config| {
                config.rate_limit_auth_per_minute = 1;
            }));

            let _spent = client.get("/v1/auth/login").send().await;
            let refused = client.get("/v1/auth/login").send().await;
            assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        });
    });

    let snapshot = snapshotter.snapshot().into_vec();

    assert_eq!(
        counter(
            &snapshot,
            "beam_http_requests_total",
            &[
                ("method", "GET"),
                ("route", "/v1/auth/login"),
                ("status", "429"),
            ],
        ),
        Some(1),
        "the rate-limited refusal must be counted; snapshot: {snapshot:?}"
    );
}

/// `on_response` reports the head; `on_disconnect` reports the body that was
/// announced and never delivered. A `TestClient` always drains, so this drives
/// the service directly and drops the response with its body unread -- which is
/// what a player abandoning a range download does.
#[test]
fn a_body_dropped_before_delivery_is_counted_as_a_disconnect() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let rt = runtime();

    metrics::with_local_recorder(&recorder, || {
        rt.block_on(async {
            // A fully-read response first, so the counter below can tell the
            // two apart rather than counting every response.
            let delivered = TestClient::new(service(None, |_| {}))
                .get("/v1/health")
                .send()
                .await;
            assert_eq!(delivered.status(), StatusCode::OK);

            let abandoned = service(None, |_| {});
            let mut request = Request::new(Body::empty());
            *request.uri_mut() = "/v1/health".parse().expect("a request target");
            let response = abandoned.call(request).await;
            assert_eq!(response.status(), StatusCode::OK);

            // The body's `Drop` is what reports; nothing polled it.
            drop(response);
        });
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let labels = [("method", "GET"), ("route", "/v1/health")];

    assert_eq!(
        counter(&snapshot, "beam_http_response_disconnects_total", &labels),
        Some(1),
        "only the abandoned body counts as a disconnect; snapshot: {snapshot:?}"
    );
    assert!(
        histogram_recorded(&snapshot, "beam_http_response_delivery_seconds", &labels),
        "an abandoned body still reports how long it lasted"
    );
}
