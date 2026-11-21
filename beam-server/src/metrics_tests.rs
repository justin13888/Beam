//! The recorder's configuration, asserted on what it renders.
//!
//! No test installs a global recorder: each builds the production
//! configuration with `build_recorder` and records through it with
//! `metrics::with_local_recorder`, so parallel tests cannot collide.

use std::collections::BTreeSet;
use std::path::Path;

use super::{
    API_LATENCY_BUCKETS, DESCRIPTIONS, Kind, LONG_OPERATION_BUCKETS, LONG_OPERATION_HISTOGRAMS,
    UPKEEP_INTERVAL, Upkeep, builder, register, spawn_upkeep,
};

/// Render the production recorder after `record` ran against it, with every
/// description registered and the build recorded by the same [`register`]
/// `install` calls.
fn render(record: impl FnOnce()) -> String {
    let recorder = builder()
        .expect("the bucket configuration is valid")
        .build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, || {
        register();
        record();
    });
    handle.render()
}

// ─── upkeep ─────────────────────────────────────────────────────────────────

/// Counts how often it was asked to drain.
#[derive(Clone, Default)]
struct CountingUpkeep(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl CountingUpkeep {
    fn runs(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Upkeep for CountingUpkeep {
    fn run_upkeep(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Lets the spawned task run to its next await without moving time.
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// With nothing scraping, upkeep is the only thing that drains histograms, so
/// it must keep running once per interval -- not once, and not in a busy loop.
#[tokio::test(start_paused = true)]
async fn upkeep_drains_once_at_start_and_once_per_interval_after() {
    let upkeep = CountingUpkeep::default();
    let task = spawn_upkeep(upkeep.clone());

    settle().await;
    assert_eq!(upkeep.runs(), 1, "upkeep runs as soon as it starts");

    tokio::time::advance(UPKEEP_INTERVAL - std::time::Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(upkeep.runs(), 1, "nothing more before the interval elapses");

    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(upkeep.runs(), 2, "one more drain at the interval");

    tokio::time::advance(UPKEEP_INTERVAL * 3).await;
    settle().await;
    assert_eq!(
        upkeep.runs(),
        3,
        "a late wake-up drains once rather than catching up on missed ticks"
    );

    tokio::time::advance(UPKEEP_INTERVAL).await;
    settle().await;
    assert_eq!(upkeep.runs(), 4, "and keeps going for as long as it runs");

    task.abort();
}

/// The `le` bounds of `name`'s bucket series, in rendered order, without `+Inf`.
fn bucket_bounds(exposition: &str, name: &str) -> Vec<f64> {
    let prefix = format!("{name}_bucket{{");
    exposition
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .filter_map(|line| {
            let (_, rest) = line.split_once("le=\"")?;
            let (bound, _) = rest.split_once('"')?;
            (bound != "+Inf").then(|| bound.parse().expect("a numeric bucket bound"))
        })
        .collect()
}

#[test]
fn every_seconds_histogram_renders_as_a_bucketed_histogram_with_help() {
    let exposition = render(|| {
        ::metrics::histogram!("beam_http_request_duration_seconds", "route" => "/v1/health")
            .record(0.03);
        ::metrics::histogram!("beam_http_response_delivery_seconds", "route" => "/v1/stream")
            .record(120.0);
    });

    for name in [
        "beam_http_request_duration_seconds",
        "beam_http_response_delivery_seconds",
    ] {
        assert!(
            exposition.contains(&format!("# TYPE {name} histogram")),
            "{name} must render as a histogram, not a summary:\n{exposition}"
        );
        assert!(
            exposition.contains(&format!("# HELP {name} ")),
            "{name} must carry its HELP line:\n{exposition}"
        );
        assert!(
            !exposition.contains(&format!("{name}{{quantile=")),
            "{name} must not carry summary quantiles:\n{exposition}"
        );
    }

    assert_eq!(
        bucket_bounds(&exposition, "beam_http_request_duration_seconds"),
        API_LATENCY_BUCKETS,
        "a request-latency histogram gets the API latency buckets"
    );
    assert_eq!(
        bucket_bounds(&exposition, "beam_http_response_delivery_seconds"),
        LONG_OPERATION_BUCKETS,
        "a long-operation histogram gets the long-operation buckets, not the suffix default"
    );
}

#[test]
fn the_build_is_identified_by_a_gauge_of_one_carrying_the_package_version() {
    let exposition = render(|| {});

    let line = exposition
        .lines()
        .find(|line| line.starts_with("beam_build_info{"))
        .unwrap_or_else(|| panic!("beam_build_info must be present:\n{exposition}"));
    assert!(
        line.contains(&format!("version=\"{}\"", env!("CARGO_PKG_VERSION"))),
        "the version label is the package version: {line}"
    );
    assert!(
        line.contains("commit=\""),
        "a commit label is present: {line}"
    );
    assert!(line.ends_with(" 1"), "the gauge is 1: {line}");
    assert!(
        exposition.contains("# HELP beam_build_info "),
        "beam_build_info carries its HELP line:\n{exposition}"
    );
}

/// The platform the server ships on; elsewhere the collector may report
/// nothing, which is its documented behaviour rather than a defect here.
#[cfg(target_os = "linux")]
#[test]
fn a_render_after_collecting_carries_the_standard_process_series() {
    let exposition = render(super::collect_process);

    for name in [
        "process_cpu_seconds_total",
        "process_resident_memory_bytes",
        "process_open_fds",
        "process_start_time_seconds",
    ] {
        assert!(
            exposition
                .lines()
                .any(|line| line.starts_with(&format!("{name} "))),
            "{name} must be sampled:\n{exposition}"
        );
        assert!(
            exposition.contains(&format!("# HELP {name} ")),
            "{name} must carry its HELP line:\n{exposition}"
        );
    }
}

#[test]
fn every_long_operation_histogram_is_a_described_metric() {
    for name in LONG_OPERATION_HISTOGRAMS {
        assert!(
            DESCRIPTIONS
                .iter()
                .any(|description| description.name == *name && description.kind == Kind::Histogram),
            "{name} is given buckets but is not a described histogram"
        );
    }
}

// ─── descriptions match what the code emits ─────────────────────────────────

/// Every `beam_` metric name passed as a literal to `counter!`, `gauge!` or
/// `histogram!` in `source`.
fn emitted_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for macro_name in ["counter!(", "gauge!(", "histogram!("] {
        for (start, _) in source.match_indices(macro_name) {
            let rest = source[start + macro_name.len()..].trim_start();
            let Some(literal) = rest.strip_prefix('"') else {
                continue;
            };
            let Some((name, _)) = literal.split_once('"') else {
                continue;
            };
            if name.starts_with("beam_") {
                names.push(name.to_owned());
            }
        }
    }
    names
}

/// Every non-test `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, into: &mut Vec<std::path::PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_sources(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_tests.rs") || name == "tests.rs")
        {
            into.push(path);
        }
    }
}

/// The names every workspace crate's production sources emit.
fn workspace_emitted_names() -> BTreeSet<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("beam-server sits inside the workspace");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(workspace).expect("the workspace root is readable") {
        let crate_dir = entry.expect("a directory entry").path();
        let is_rust_crate = crate_dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("beam-"))
            && crate_dir.join("Cargo.toml").is_file()
            && crate_dir.join("src").is_dir();
        if is_rust_crate {
            rust_sources(&crate_dir.join("src"), &mut files);
        }
    }
    files
        .iter()
        .flat_map(|file| {
            let source = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("reading {}: {error}", file.display()));
            emitted_names(&source)
        })
        .collect()
}

#[test]
fn every_emitted_metric_is_described_and_every_description_is_emitted() {
    let emitted = workspace_emitted_names();
    let described: BTreeSet<String> = DESCRIPTIONS
        .iter()
        .map(|description| description.name.to_owned())
        .collect();

    // Guards the scanner itself: a scan that found nothing would make both
    // comparisons below vacuous.
    assert!(
        emitted.contains("beam_http_requests_total"),
        "the scan must find the HTTP observer's counter; found {emitted:?}"
    );

    let undescribed: Vec<_> = emitted.difference(&described).collect();
    assert!(
        undescribed.is_empty(),
        "emitted without a DESCRIPTIONS entry: {undescribed:?}"
    );
    let stale: Vec<_> = described.difference(&emitted).collect();
    assert!(stale.is_empty(), "described but emitted nowhere: {stale:?}");
}

// ─── the Helm chart's alerts and dashboard name only described metrics ──────

/// Every identifier in `text` that starts `beam_` or `process_` at a word
/// boundary: the metric names a PromQL expression or a dashboard query uses.
fn referenced_names(text: &str) -> BTreeSet<String> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut names = BTreeSet::new();
    for prefix in ["beam_", "process_"] {
        for (start, _) in text.match_indices(prefix) {
            if text[..start].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let end = text[start..]
                .find(|c: char| !is_ident(c))
                .map_or(text.len(), |offset| start + offset);
            names.insert(text[start..end].to_owned());
        }
    }
    names
}

/// The chart files whose queries name metrics: the PrometheusRule template and
/// every dashboard under `charts/beam/dashboards`.
fn chart_query_files() -> Vec<std::path::PathBuf> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("beam-server sits inside the workspace")
        .join("charts/beam");
    let mut files = vec![chart.join("templates/prometheusrule.yaml")];
    let dashboards = chart.join("dashboards");
    for entry in std::fs::read_dir(&dashboards)
        .unwrap_or_else(|error| panic!("reading {}: {error}", dashboards.display()))
    {
        let path = entry.expect("a directory entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    files
}

/// The chart's alerts and dashboard (issue #459) are only as good as the
/// series they query: a renamed or removed metric would leave an alert that
/// never fires and a panel that is silently empty. Every name they use must
/// be one this server describes -- a `DESCRIPTIONS` entry, or a `process_*`
/// series the collector renders with HELP text -- read from the server
/// itself, not from a list kept beside the chart.
///
/// Linux only, for the reason the process-series test above gives.
#[cfg(target_os = "linux")]
#[test]
fn every_metric_the_chart_queries_is_described_by_the_server() {
    let exposition = render(super::collect_process);
    let mut described: BTreeSet<String> = DESCRIPTIONS
        .iter()
        .map(|description| description.name.to_owned())
        .collect();
    described.extend(exposition.lines().filter_map(|line| {
        let name = line.strip_prefix("# HELP ")?.split_once(' ')?.0;
        name.starts_with("process_").then(|| name.to_owned())
    }));
    let histograms: BTreeSet<&str> = DESCRIPTIONS
        .iter()
        .filter(|description| description.kind == Kind::Histogram)
        .map(|description| description.name)
        .collect();

    for file in chart_query_files() {
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("reading {}: {error}", file.display()));
        let referenced = referenced_names(&text);
        assert!(
            !referenced.is_empty(),
            "{} names no metric; the scan must find its queries",
            file.display()
        );
        for name in &referenced {
            // A histogram is queried through the series the exporter derives
            // from it, never by its own name.
            let queried = ["_bucket", "_sum", "_count"]
                .iter()
                .find_map(|suffix| {
                    name.strip_suffix(suffix)
                        .filter(|base| histograms.contains(base))
                })
                .unwrap_or(name);
            assert!(
                described.contains(queried),
                "{} queries {name}, which the server does not describe",
                file.display()
            );
        }
    }
}

#[test]
fn the_chart_query_scan_finds_names_at_word_boundaries_only() {
    let names = referenced_names(
        r#"rate(beam_http_requests_total{a="b"}[5m]) / process_open_fds and my_beam_x, "beam.json""#,
    );
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        ["beam_http_requests_total", "process_open_fds"]
    );
}

#[test]
fn descriptions_name_each_metric_once() {
    let mut seen = BTreeSet::new();
    for description in DESCRIPTIONS {
        assert!(
            seen.insert(description.name),
            "{} is described twice",
            description.name
        );
        assert!(
            !description.help.is_empty(),
            "{} has no HELP text",
            description.name
        );
    }
}
