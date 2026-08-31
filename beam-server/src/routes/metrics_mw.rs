//! Prometheus metrics for the HTTP surface
//! ([#82](https://github.com/justin13888/beam/issues/82)).
//!
//! Two pieces live here:
//!
//! * [`HttpMetrics`] — an [`Observer`] recording
//!   `beam_http_requests_total{method, route, status}` and
//!   `beam_http_request_duration_seconds{method, route}`. An observer sees
//!   every response and contributes nothing to the description, which is
//!   exactly right for a metric: it must not appear in the contract, and it
//!   must count responses no handler produced. Kynos calls it outside the
//!   interceptor chain, so a rate-limited `429` and a same-origin `403` are
//!   both counted — under Salvo that depended on `HttpMetrics` being installed
//!   as the outermost hoop, and now it is structural.
//! * [`render_metrics`] — `GET /metrics`, rendering the [`PrometheusHandle`]
//!   carried on [`AppState`].
//!
//! # Route label cardinality
//!
//! The `route` label is [`Route::path`] — the `paths` key, with its `{}`
//! placeholders intact. It is bounded by the number of declared operations, so
//! the embedded UUIDs that would mint a time series per media item never reach
//! it. This replaces `classify_route`, a hand-maintained mirror of the `/v1`
//! route table that Salvo forced on us because it did not expose the matched
//! pattern to middleware. A mirror that drifts silently is the failure mode
//! AGENTS.md names; Kynos hands over the template, so the mirror is gone.
//!
//! # What `elapsed` measures
//!
//! Producing the response *head*, not delivering the body. The SSE stream and
//! ranged downloads therefore report near-zero durations here. That is not a
//! defect to work around: [`Observer::on_disconnect`] is the honest companion,
//! and `beam_http_response_disconnects_total` counts the bodies that were
//! announced and never delivered — a player abandoning a large range mid-
//! download was previously counted as served.

use std::time::Duration;

use kynos::extract::body::text::Text;
use kynos::http::{Request, Response};
use kynos::middleware::Observer;
use kynos::prelude::*;
use kynos::router::operation::Route;

use crate::routes::tags::Internal;
use crate::state::AppState;

/// What an unmatched request has instead of a `paths` key.
///
/// Spelled the way Kynos's own `Trace` spells it, so the two agree in a log
/// line and a metric read side by side. A 404 is counted; its label is this
/// constant rather than the path it asked for, which is what keeps a scanner
/// probing random URLs from minting a time series apiece.
const UNMATCHED: &str = "<unmatched>";

/// Records a counter and a latency histogram for every response.
///
/// Emits through the `metrics` facade, so with no recorder installed
/// (`BEAM_ENABLE_METRICS=false`) every call is a no-op and the observer costs
/// a branch.
pub struct HttpMetrics;

impl Observer<AppState> for HttpMetrics {
    fn on_request(&self, _request: &Request, _route: Option<Route<'_>>, _context: &AppState) {}

    fn on_response(&self, response: &Response, route: Option<Route<'_>>, elapsed: Duration) {
        let (method, path) = labels(route);

        metrics::counter!(
            "beam_http_requests_total",
            "method" => method.clone(),
            "route" => path.clone(),
            "status" => response.status().as_u16().to_string(),
        )
        .increment(1);
        metrics::histogram!(
            "beam_http_request_duration_seconds",
            "method" => method,
            "route" => path,
        )
        .record(elapsed.as_secs_f64());
    }

    fn on_disconnect(&self, route: Option<Route<'_>>, elapsed: Duration) {
        let (method, path) = labels(route);

        // Called from the body's `Drop`, so this does the little work a
        // destructor may: one counter, no blocking, no await.
        metrics::counter!(
            "beam_http_response_disconnects_total",
            "method" => method.clone(),
            "route" => path.clone(),
        )
        .increment(1);
        metrics::histogram!(
            "beam_http_response_delivery_seconds",
            "method" => method,
            "route" => path,
        )
        .record(elapsed.as_secs_f64());
    }
}

/// The `method` and `route` labels a matched or unmatched request carries.
///
/// An unmatched request is labelled `<unmatched>` on *both*, which reads odd
/// until you look at what is available: `Observer::on_response` receives the
/// response and the matched route, never the request, so when nothing matched
/// there is no method to report. Counting a 404 under a fabricated method would
/// be worse than counting it under a placeholder, and taking the method from
/// `on_request` would mean carrying per-request state through an observer that
/// is deliberately stateless. The count is the useful part; the method of a
/// request that reached no operation is not.
fn labels(route: Option<Route<'_>>) -> (String, String) {
    route.map_or_else(
        || (UNMATCHED.to_owned(), UNMATCHED.to_owned()),
        |route| (route.method().to_string(), route.path().to_owned()),
    )
}

/// No recorder is installed, so there is nothing to render.
///
/// A declared `503` rather than an unmounted route: the router's shape -- and
/// therefore the exported description -- must not depend on deployment
/// configuration, or the document stops describing every deployment it claims
/// to.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MetricsUnavailable {
    #[error("metrics are disabled; set BEAM_ENABLE_METRICS=true to install a recorder")]
    #[problem(
        status = 503,
        type = "https://beam.justinchung.net/reference/errors/metrics-unavailable",
        title = "Metrics unavailable"
    )]
    NoRecorder,
}

/// Prometheus text exposition of everything the installed recorder collected.
///
/// Unauthenticated, and deliberately so: it is only as reachable as
/// `BEAM_BIND_ADDRESS`, and the supported deployment keeps that internal behind
/// a reverse proxy which does not forward `/metrics` (see
/// `docs/operations/deployment.md`).
///
/// Described rather than hidden. Salvo let this be a plain `Handler` that
/// `merge_router` could not see, so it stayed out of the spec by accident of
/// the framework. Kynos routes and describes from one declaration, and the only
/// ways to keep it out would be the `unchecked` feature -- which stamps the
/// *whole* document non-authoritative to hide one route -- or a second listener
/// on its own port. Describing it under an `internal` tag is the honest option,
/// and satisfies the readiness contract's requirement that internal routes be
/// explicitly marked.
#[kynos::get("/metrics", tag = Internal, operation_id = "renderMetrics")]
pub async fn render_metrics(Inject(state): Inject<AppState>) -> Result<Text, MetricsUnavailable> {
    state
        .metrics()
        .map(|handle| Text(handle.render()))
        .ok_or(MetricsUnavailable::NoRecorder)
}

#[cfg(test)]
#[path = "metrics_mw_tests.rs"]
mod metrics_mw_tests;
