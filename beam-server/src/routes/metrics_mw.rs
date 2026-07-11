//! Prometheus metrics for the HTTP surface, mounted only when
//! `BEAM_ENABLE_METRICS=true` ([#82](https://github.com/justin13888/beam/issues/82)).
//!
//! Two pieces live here:
//!
//! * [`HttpMetrics`] — a hoop installed as the *outermost* middleware around
//!   the `/v1` subtree, recording `beam_http_requests_total{method, route,
//!   status}` and `beam_http_request_duration_seconds{method, route}` for
//!   every matched request. Wrapping the whole subtree means rate-limited
//!   `429`s and same-origin rejections are counted too. It emits through the
//!   `metrics` facade, so with no recorder installed (metrics disabled — the
//!   hoop is then not mounted anyway) every call is a no-op.
//! * [`MetricsEndpoint`] — the `GET /metrics` handler rendering the
//!   [`PrometheusHandle`]'s text exposition. Mounted at the top level
//!   (outside `/v1`) and **unauthenticated**: the endpoint is only as
//!   reachable as `BEAM_BIND_ADDRESS`, and the supported deployment keeps
//!   that internal behind the reverse proxy, which does not forward
//!   `/metrics` (see `docs/operations/deployment.md`). It is deliberately a
//!   plain [`Handler`] (not `#[endpoint]`) so it can never leak into the
//!   OpenAPI spec.
//!
//! # Route label cardinality
//!
//! Prometheus label sets must stay bounded, so the `route` label is a
//! deterministic class from [`classify_route`] — never the raw path, whose
//! embedded UUIDs would mint a new time series per media item.

use std::time::Instant;

use metrics_exporter_prometheus::PrometheusHandle;
use salvo::http::Method;
use salvo::prelude::*;

/// Classify a request path into a low-cardinality `route` label.
///
/// Salvo 0.89 does not expose the matched route pattern to middleware, so
/// this mirrors the `/v1` route table in `rest_routes` by hand. Anything
/// unrecognized (including future routes until they are added here) falls
/// back to `"other"` rather than emitting an unbounded value.
pub fn classify_route(_method: &Method, path: &str) -> &'static str {
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    if segments.next() != Some("v1") {
        return "other";
    }

    match (segments.next(), segments.next(), segments.next()) {
        (Some("health"), None, None) => "health",
        (Some("media"), None, None) => "media_browse",
        (Some("media"), Some(_), None) => "media_detail",
        (Some("media"), Some(_), Some("sources")) => "media_sources",
        (Some("genres"), None, None) => "genres",
        (Some("files"), Some(_), Some("stream")) => "stream",
        (Some("files"), Some(_), Some("download")) => "download",
        (Some("files"), Some(_), Some("progress")) => "progress",
        (Some("continue-watching"), None, None) => "continue_watching",
        (Some("history"), None, None) => "history",
        (Some("libraries"), _, _) => "libraries",
        (Some("admin"), _, _) => "admin",
        (Some("auth"), _, _) => "auth",
        (Some("me"), None, None)
        | (Some("logout"), None, None)
        | (Some("logout-all"), None, None)
        | (Some("sessions"), _, _) => "session",
        _ => "other",
    }
}

/// Outermost `/v1` hoop: counts every request and times it, labelled by
/// method, [`classify_route`] class, and final response status.
pub struct HttpMetrics;

#[async_trait::async_trait]
impl Handler for HttpMetrics {
    async fn handle(
        &self,
        req: &mut Request,
        depot: &mut Depot,
        res: &mut Response,
        ctrl: &mut FlowCtrl,
    ) {
        let method = req.method().as_str().to_string();
        let route = classify_route(req.method(), req.uri().path());
        let start = Instant::now();

        ctrl.call_next(req, depot, res).await;

        // Salvo leaves the status unset for a plain-200 response; anything
        // that deviates (errors, redirects, 429s) sets it explicitly.
        let status = res
            .status_code
            .unwrap_or(salvo::http::StatusCode::OK)
            .as_u16()
            .to_string();

        metrics::counter!(
            "beam_http_requests_total",
            "method" => method.clone(),
            "route" => route,
            "status" => status,
        )
        .increment(1);
        metrics::histogram!(
            "beam_http_request_duration_seconds",
            "method" => method,
            "route" => route,
        )
        .record(start.elapsed().as_secs_f64());
    }
}

/// `GET /metrics`: renders the Prometheus text exposition of everything the
/// installed recorder has collected.
pub struct MetricsEndpoint {
    handle: PrometheusHandle,
}

impl MetricsEndpoint {
    pub fn new(handle: PrometheusHandle) -> Self {
        Self { handle }
    }
}

#[async_trait::async_trait]
impl Handler for MetricsEndpoint {
    async fn handle(
        &self,
        _req: &mut Request,
        _depot: &mut Depot,
        res: &mut Response,
        _ctrl: &mut FlowCtrl,
    ) {
        res.render(Text::Plain(self.handle.render()));
    }
}

#[cfg(test)]
#[path = "metrics_mw_tests.rs"]
mod metrics_mw_tests;
