//! What an operator deploys and watches: `/metrics` under
//! `BEAM_ENABLE_METRICS`, the rate limiter under `BEAM_RATE_LIMIT_*` and
//! `BEAM_TRUSTED_PROXIES`, and the admin event stream during a real scan.

use std::time::Duration;

use beam_test_support::e2e::StackOptions;
use beam_test_support::e2e::client::api;

use crate::common::{Admin, VIEWER, Video, assert_refused, header, raw_get_with};

/// How long the admin event stream may take to report a whole scan.
const STREAM_BOUND: Duration = Duration::from_secs(120);

/// `GET /v1/media` -- the one search-limited route -- as the client at
/// `forwarded_for` behind the local proxy, with `admin`'s session.
async fn browse_as(
    admin: &Admin,
    forwarded_for: &str,
) -> beam_test_support::e2e::reqwest::Response {
    raw_get_with(
        admin.stack.base_url(),
        "/v1/media",
        Some(&admin.session),
        &[("x-forwarded-for", forwarded_for)],
    )
    .await
}

/// The value of the `beam_http_requests_total` series for `route` and
/// `status` in a Prometheus exposition, or 0 where it has none.
fn requests_total(exposition: &str, route: &str, status: u16) -> f64 {
    let route = format!("route=\"{route}\"");
    let status = format!("status=\"{status}\"");
    exposition
        .lines()
        .filter(|line| line.starts_with("beam_http_requests_total{"))
        .filter(|line| line.contains(&route) && line.contains(&status))
        .map(|line| {
            line.rsplit(' ')
                .next()
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or_else(|| panic!("a sample line ends in its value: {line}"))
        })
        .sum()
}

/// Behind a proxy the operator trusts, each client the proxy names has a
/// search budget of its own; past it the search is refused with `429` and a
/// `Retry-After`, while the routes outside the limiter are not; and the
/// metrics endpoint, enabled, counts the refusal under the route it refused.
#[tokio::test]
async fn trusted_proxy_clients_have_their_own_search_budget_and_metrics_count_the_refusals() {
    let admin = Admin::start_with(
        StackOptions::new()
            .env("BEAM_ENABLE_METRICS", "true")
            .env("BEAM_RATE_LIMIT_SEARCH_PER_MINUTE", "2")
            .env("BEAM_TRUSTED_PROXIES", "127.0.0.1/32"),
    )
    .await;

    let first = "203.0.113.7";
    for n in 1..=2 {
        assert_eq!(
            browse_as(&admin, first).await.status().as_u16(),
            200,
            "search {n} of a budget of 2"
        );
    }
    let refused = browse_as(&admin, first).await;
    assert_eq!(refused.status().as_u16(), 429, "the budget is spent");
    let retry_after: u64 = header(&refused, "retry-after")
        .expect("a refusal says when to come back")
        .parse()
        .expect("Retry-After in seconds");
    assert!(retry_after > 0, "a spent budget is not refilled at once");

    assert_eq!(
        browse_as(&admin, "203.0.113.8").await.status().as_u16(),
        200,
        "another client behind the same proxy has a budget of its own"
    );
    for _ in 0..3 {
        admin
            .api
            .list_libraries()
            .await
            .expect("a route outside the limiter is not limited");
    }

    let metrics = raw_get_with(admin.stack.base_url(), "/metrics", None, &[]).await;
    assert_eq!(metrics.status().as_u16(), 200, "metrics are enabled");
    let exposition = metrics.text().await.expect("the exposition");
    assert_eq!(
        requests_total(&exposition, "/v1/media", 200),
        3.0,
        "every search served is counted:\n{exposition}"
    );
    assert_eq!(
        requests_total(&exposition, "/v1/media", 429),
        1.0,
        "the limiter's refusal is counted under the route it refused:\n{exposition}"
    );

    admin.finish().await;
}

/// With no trusted proxy configured -- the default -- `X-Forwarded-For` is
/// believed from nobody: every client the header names spends the one
/// budget of the socket's peer. Metrics left off are a `503`, not a `404`.
#[tokio::test]
async fn an_untrusted_forwarded_for_shares_one_budget_and_metrics_are_off_by_default() {
    let admin =
        Admin::start_with(StackOptions::new().env("BEAM_RATE_LIMIT_SEARCH_PER_MINUTE", "2")).await;

    assert_eq!(
        browse_as(&admin, "203.0.113.7").await.status().as_u16(),
        200
    );
    assert_eq!(
        browse_as(&admin, "203.0.113.8").await.status().as_u16(),
        200
    );
    assert_eq!(
        browse_as(&admin, "203.0.113.9").await.status().as_u16(),
        429,
        "a header naming a new client does not buy a new budget"
    );

    let metrics = raw_get_with(admin.stack.base_url(), "/metrics", None, &[]).await;
    assert_eq!(
        metrics.status().as_u16(),
        503,
        "metrics are off unless enabled"
    );

    admin.finish().await;
}

/// An administrator's event stream reports a real scan as it runs: the job
/// started, then finished with what it found, every event naming the library
/// it is about. The stream is announced so a proxy neither buffers nor
/// caches it, and is an administrator's alone.
#[tokio::test]
async fn the_admin_event_stream_reports_a_scan_as_it_runs() {
    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    let library = admin.add_library("Films", "films").await;

    // What a proxy is told about the stream, read from a stream of its own.
    let announced = raw_get_with(
        admin.stack.base_url(),
        "/v1/admin/events/stream",
        Some(&admin.session),
        &[],
    )
    .await;
    assert_eq!(announced.status().as_u16(), 200);
    assert!(
        header(&announced, "content-type").is_some_and(|t| t.starts_with("text/event-stream")),
        "{:?}",
        announced.headers()
    );
    assert_eq!(header(&announced, "cache-control"), Some("no-cache"));
    assert_eq!(header(&announced, "x-accel-buffering"), Some("no"));
    drop(announced);

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    assert_refused(
        "a viewer opening the admin event stream",
        403,
        // The stream itself is not `Debug`; only whether it opened matters.
        viewer.stream_admin_events().await.map(drop),
    );

    // Subscribed before the scan starts, so nothing it reports is missed.
    let mut events = admin
        .api
        .stream_admin_events()
        .await
        .expect("open the admin event stream");
    let job = admin
        .api
        .scan_library(library)
        .await
        .expect("start a scan")
        .into_inner()
        .id;

    let reported = tokio::time::timeout(STREAM_BOUND, async {
        let mut phases = Vec::new();
        loop {
            let event = events
                .next()
                .await
                .expect("the stream stays open while the server runs")
                .expect("each event decodes as the document describes it");
            let Some(scan) = event.scan.as_ref() else {
                continue;
            };
            if scan.job_id != job {
                continue;
            }
            assert_eq!(event.category, api::types::AdminEventCategory::ScanProgress);
            assert_eq!(
                (event.library_id, event.library_name.as_deref()),
                (Some(library), Some("Films")),
                "{event:?}"
            );
            phases.push(scan.phase.clone());
            if matches!(
                scan.phase,
                api::types::ScanPhase::Completed | api::types::ScanPhase::Failed
            ) {
                return (phases, scan.progress.clone());
            }
        }
    })
    .await;
    let (phases, progress) = reported.unwrap_or_else(|_| {
        panic!("the stream reported no end of the scan within {STREAM_BOUND:?}")
    });
    assert_eq!(
        phases.first(),
        Some(&api::types::ScanPhase::Started),
        "{phases:?}"
    );
    assert_eq!(
        phases.last(),
        Some(&api::types::ScanPhase::Completed),
        "{phases:?}"
    );
    assert_eq!(
        (progress.added_count, progress.failed_count),
        (2, 0),
        "the end reports what the scan found: {progress:?}"
    );

    // The stream's account and the job's agree.
    let finished = admin.finished_scan(library, job).await;
    assert_eq!(finished.state, api::types::ScanState::Succeeded);
    assert_eq!(finished.progress.added_count, progress.added_count);

    admin.finish().await;
}
