//! The recorder's configuration, asserted on what it renders.
//!
//! No test installs a global recorder: each builds the production
//! configuration with `build_recorder` and records through it with
//! `metrics::with_local_recorder`, so parallel tests cannot collide.

use std::collections::BTreeSet;
use std::path::Path;

use super::{
    API_LATENCY_BUCKETS, DESCRIPTIONS, Kind, LONG_OPERATION_BUCKETS, LONG_OPERATION_HISTOGRAMS,
    builder, describe, record_build_info,
};

/// Render the production recorder after `record` ran against it, with every
/// description registered and the build recorded, as `install` does.
fn render(record: impl FnOnce()) -> String {
    let recorder = builder()
        .expect("the bucket configuration is valid")
        .build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, || {
        describe();
        record_build_info();
        record();
    });
    handle.render()
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
