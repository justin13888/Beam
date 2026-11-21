//! The process-wide Prometheus recorder: how it is built, what every metric
//! means, and the upkeep that keeps it bounded
//! ([#456](https://github.com/justin13888/beam/issues/456)).
//!
//! Everything that emits a metric -- the HTTP observer, beam-index's domain
//! counters -- goes through the `metrics` facade and never sees this module.
//! What lives here is what the *recorder* needs to know about those metrics:
//!
//! * **Buckets.** Without them the exporter renders every `histogram!` as a
//!   Prometheus `summary`, whose precomputed quantiles cannot be summed across
//!   pods or fed to `histogram_quantile`. [`builder`] gives every `*_seconds`
//!   histogram real buckets.
//! * **Descriptions.** [`DESCRIPTIONS`] is the one place a metric's HELP text
//!   and unit are written; [`describe`] registers them all at startup.
//! * **Upkeep.** `install_recorder` leaves draining histogram samples to the
//!   caller. Without [`spawn_upkeep`] they are drained only on render, so with
//!   metrics on and nothing scraping they grow without bound.
//! * **Process metrics.** The standard unprefixed `process_*` series, which
//!   [`collect_process`] samples each time `/metrics` is rendered.

use std::sync::LazyLock;
use std::time::Duration;

use ::metrics::{KeyName, SharedString, Unit};
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};

/// Upper bounds, in seconds, for anything that answers a request: tuned for
/// API latency, from a cache hit to a slow search.
pub const API_LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Upper bounds, in seconds, for operations that last as long as a client
/// keeps reading: a range download or an event stream, from a quick abandon to
/// a feature-length film.
pub const LONG_OPERATION_BUCKETS: &[f64] = &[
    0.1, 0.5, 1.0, 5.0, 15.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0, 14400.0,
];

/// The histograms measured in [`LONG_OPERATION_BUCKETS`]; every other
/// `*_seconds` histogram gets [`API_LATENCY_BUCKETS`].
///
/// An exact-name matcher takes precedence over the suffix matcher in the
/// exporter, so listing a name here is all it takes to move it.
pub const LONG_OPERATION_HISTOGRAMS: &[&str] = &[
    "beam_http_response_delivery_seconds",
    "beam_scan_duration_seconds",
];

/// How often [`spawn_upkeep`] drains histogram samples.
///
/// The exporter's own listener uses the same interval; it bounds what an
/// unscraped histogram holds to five seconds of samples.
pub const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

/// The `commit` label of `beam_build_info`, when the build supplied one.
///
/// Read at compile time from `BEAM_GIT_COMMIT`; a build that did not set it
/// reports `unknown` rather than failing, so a local `cargo build` still works.
const BUILD_COMMIT: &str = match option_env!("BEAM_GIT_COMMIT") {
    Some(commit) => commit,
    None => "unknown",
};

/// Which `describe_*` a metric is registered through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}

/// One metric's HELP line and unit.
#[derive(Clone, Copy, Debug)]
pub struct Description {
    pub name: &'static str,
    pub kind: Kind,
    pub unit: Option<Unit>,
    pub help: &'static str,
}

/// Every metric this process emits under the `beam_` prefix.
///
/// A test scans the workspace's sources for every `counter!`, `gauge!` and
/// `histogram!` naming a `beam_` metric and requires an entry here, and the
/// reverse, so a new metric without a description -- or a description left
/// behind by a removed metric -- fails the build rather than drifting.
pub const DESCRIPTIONS: &[Description] = &[
    Description {
        name: "beam_build_info",
        kind: Kind::Gauge,
        unit: None,
        help: "Always 1; the version and commit labels identify the running build.",
    },
    Description {
        name: "beam_http_requests_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "HTTP responses produced, by method, route template and status.",
    },
    Description {
        name: "beam_http_request_duration_seconds",
        kind: Kind::Histogram,
        unit: Some(Unit::Seconds),
        help: "Time from a request arriving to its response head being produced, by method and route template.",
    },
    Description {
        name: "beam_http_requests_in_flight",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "HTTP requests currently being handled, up to their response head.",
    },
    Description {
        name: "beam_http_response_disconnects_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Response bodies dropped before their end, by method and route template.",
    },
    Description {
        name: "beam_http_response_delivery_seconds",
        kind: Kind::Histogram,
        unit: Some(Unit::Seconds),
        help: "Time from a request arriving to its undelivered response body being dropped, by method and route template.",
    },
    Description {
        name: "beam_dependency_up",
        kind: Kind::Gauge,
        unit: None,
        help: "1 when the dependency's last /v1/health probe passed, 0 when it failed.",
    },
    Description {
        name: "beam_index_files_processed_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Files the indexer processed, by result.",
    },
    Description {
        name: "beam_index_divergence_warnings_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Renditions of one title whose runtimes diverged past the warning threshold.",
    },
    Description {
        name: "beam_enrichment_outcomes_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Metadata-enrichment attempts, by outcome.",
    },
    Description {
        name: "beam_scan_duration_seconds",
        kind: Kind::Histogram,
        unit: Some(Unit::Seconds),
        help: "Time a successful library scan ran for, from taking the library to finishing, by library id and trigger.",
    },
    Description {
        name: "beam_scan_last_success_timestamp_seconds",
        kind: Kind::Gauge,
        unit: Some(Unit::Seconds),
        help: "Unix time the library's last successful scan finished, by library id; 0 once the library is deleted.",
    },
    Description {
        name: "beam_scan_failures_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Library scans that failed or were interrupted, not counting those cancelled by the library's delete, by library id.",
    },
    Description {
        name: "beam_scan_in_progress",
        kind: Kind::Gauge,
        unit: None,
        help: "1 while a scan of the library is running, 0 otherwise, by library id.",
    },
    Description {
        name: "beam_watcher_events_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Filesystem change events the watcher attributed to a library, by library id and kind.",
    },
    Description {
        name: "beam_watcher_errors_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Filesystem watcher errors, and watches that could not be added, by library id.",
    },
    Description {
        name: "beam_watcher_mode",
        kind: Kind::Gauge,
        unit: None,
        help: "1 for how the library's changes are observed (native events or polling), 0 for the other, both 0 when it is not watched; by library id and mode.",
    },
    Description {
        name: "beam_watcher_pending_paths",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "Changed paths the watcher holds and has not reconciled yet: being debounced, or waiting in the batch being reconciled.",
    },
    Description {
        name: "beam_watcher_deferred_paths",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "Changed paths whose reconcile was deferred -- the library was being scanned, or the file was still being written -- held for a retry.",
    },
    Description {
        name: "beam_provider_requests_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Requests to a metadata provider, by provider and result.",
    },
    Description {
        name: "beam_provider_request_duration_seconds",
        kind: Kind::Histogram,
        unit: Some(Unit::Seconds),
        help: "Time a metadata provider took to answer a request, by provider.",
    },
    Description {
        name: "beam_provider_rate_limited_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Requests a metadata provider refused for its rate limit, by provider.",
    },
    Description {
        name: "beam_enrichment_pending",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "Titles waiting for metadata enrichment, due or backing off, as of the last sweep.",
    },
    Description {
        name: "beam_library_files",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "Files present in the library as of its last completed scan, by library id; 0 once the library is deleted.",
    },
    Description {
        name: "beam_trickplay_pending",
        kind: Kind::Gauge,
        unit: Some(Unit::Count),
        help: "Files the running trickplay pass has still to make sheets for.",
    },
    Description {
        name: "beam_trickplay_failures_total",
        kind: Kind::Counter,
        unit: Some(Unit::Count),
        help: "Files whose trickplay sheets could not be made: left for a later pass, or undecodable.",
    },
];

/// The recorder's configuration: real buckets for every `*_seconds` histogram.
///
/// Separate from [`install`] so a test can build the same recorder locally
/// with `build_recorder` instead of installing a global one.
pub fn builder() -> Result<PrometheusBuilder, BuildError> {
    let mut builder = PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_seconds".to_owned()), API_LATENCY_BUCKETS)?;
    for name in LONG_OPERATION_HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full((*name).to_owned()), LONG_OPERATION_BUCKETS)?;
    }
    Ok(builder)
}

/// Install the global recorder, describe every metric, and record the build.
///
/// Called once, at startup, when `BEAM_ENABLE_METRICS` is on. The caller still
/// owns upkeep: hand the returned handle to [`spawn_upkeep`].
pub fn install() -> Result<PrometheusHandle, BuildError> {
    let handle = builder()?.install_recorder()?;
    register();
    Ok(handle)
}

/// Everything [`install`] does to a recorder once it is current: describe
/// every metric and record the build.
///
/// Separate so a test can run exactly this against a local recorder; it is
/// the one sequence `install` and that test share.
pub fn register() {
    describe();
    record_build_info();
}

/// Register every entry of [`DESCRIPTIONS`] with the current recorder.
pub fn describe() {
    ::metrics::with_recorder(|recorder| {
        for &Description {
            name,
            kind,
            unit,
            help,
        } in DESCRIPTIONS
        {
            let key = KeyName::from_const_str(name);
            let help = SharedString::const_str(help);
            match kind {
                Kind::Counter => recorder.describe_counter(key, unit, help),
                Kind::Gauge => recorder.describe_gauge(key, unit, help),
                Kind::Histogram => recorder.describe_histogram(key, unit, help),
            }
        }
    });
    PROCESS.describe();
}

/// The collector behind the `process_*` series.
///
/// One per process: it caches the metric names it allocates, and the process it
/// reads is the one it lives in.
static PROCESS: LazyLock<metrics_process::Collector> =
    LazyLock::new(metrics_process::Collector::default);

/// Sample CPU time, memory, file descriptors, threads and start time into the
/// current recorder.
///
/// Called on every render of `/metrics` rather than on a timer, so a scrape
/// reads the process as it is at the scrape. The reads are a handful of small
/// `/proc` files on Linux; a platform the crate cannot read reports nothing.
pub fn collect_process() {
    PROCESS.collect();
}

/// Set `beam_build_info{version, commit}` to 1.
pub fn record_build_info() {
    ::metrics::gauge!(
        "beam_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "commit" => BUILD_COMMIT,
    )
    .set(1.0);
}

/// A recorder whose histogram samples must be drained periodically.
///
/// The seam [`spawn_upkeep`] runs against, so its schedule can be tested
/// without a global recorder.
pub trait Upkeep: Send + 'static {
    /// Drain buffered histogram samples.
    fn run_upkeep(&self);
}

impl Upkeep for PrometheusHandle {
    fn run_upkeep(&self) {
        PrometheusHandle::run_upkeep(self);
    }
}

/// Drain histogram samples every [`UPKEEP_INTERVAL`] for as long as the
/// process runs, starting at once.
pub fn spawn_upkeep(target: impl Upkeep) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(UPKEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            target.run_upkeep();
        }
    })
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
