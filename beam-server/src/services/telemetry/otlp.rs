//! Encoding a [`LibraryReport`] as an OTLP/HTTP JSON metrics request
//! (issue #93, ADR-0019).
//!
//! Hand-written rather than built through the OpenTelemetry SDK: the report is
//! one request a week of a dozen gauges, and the SDK's periodic reader,
//! exporter and runtime are machinery for a stream Beam does not have. What
//! matters is that the bytes are a pure function of the report -- the admin
//! preview shows exactly what is sent -- and a batching SDK cannot promise
//! that.
//!
//! The shape follows the OTLP protobuf-JSON mapping: camelCase field names,
//! 64-bit integers as decimal strings, attributes as `{key, value}` pairs.

use chrono::{NaiveTime, TimeZone, Utc};
use serde::Serialize;

use crate::models::telemetry::{LibraryReport, LibraryReportCount};

/// The `Content-Type` of an OTLP/HTTP JSON request.
pub const OTLP_JSON_CONTENT_TYPE: &str = "application/json";

/// `service.name` on every report.
const SERVICE_NAME: &str = "beam-server";

/// The instrumentation scope the gauges belong to.
const SCOPE_NAME: &str = "beam.library_report";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMetricsServiceRequest<'a> {
    resource_metrics: [ResourceMetrics<'a>; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceMetrics<'a> {
    resource: Resource<'a>,
    scope_metrics: [ScopeMetrics<'a>; 1],
}

#[derive(Serialize)]
struct Resource<'a> {
    attributes: Vec<KeyValue<'a>>,
}

#[derive(Serialize)]
struct Scope<'a> {
    name: &'a str,
    version: String,
}

#[derive(Serialize)]
struct ScopeMetrics<'a> {
    scope: Scope<'a>,
    metrics: Vec<Metric<'a>>,
}

#[derive(Serialize)]
struct Metric<'a> {
    name: &'static str,
    description: &'static str,
    unit: &'static str,
    gauge: Gauge<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Gauge<'a> {
    data_points: Vec<NumberDataPoint<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberDataPoint<'a> {
    attributes: Vec<KeyValue<'a>>,
    time_unix_nano: String,
    as_int: String,
}

#[derive(Serialize)]
struct KeyValue<'a> {
    key: &'static str,
    value: AnyValue<'a>,
}

#[derive(Serialize)]
enum AnyValue<'a> {
    #[serde(rename = "stringValue")]
    String(&'a str),
    #[serde(rename = "intValue")]
    Int(String),
}

fn text<'a>(key: &'static str, value: &'a str) -> KeyValue<'a> {
    KeyValue {
        key,
        value: AnyValue::String(value),
    }
}

/// One gauge's points, all stamped with the report's day.
struct Points<'a> {
    time_unix_nano: &'a str,
    points: Vec<NumberDataPoint<'a>>,
}

impl<'a> Points<'a> {
    fn new(time_unix_nano: &'a str) -> Self {
        Self {
            time_unix_nano,
            points: Vec::new(),
        }
    }

    fn point(mut self, attributes: Vec<KeyValue<'a>>, value: u64) -> Self {
        self.points.push(NumberDataPoint {
            attributes,
            time_unix_nano: self.time_unix_nano.to_string(),
            as_int: value.to_string(),
        });
        self
    }

    fn named(
        self,
        key: &'static str,
        counts: &'a [LibraryReportCount],
        extra: impl Fn() -> Vec<KeyValue<'a>>,
    ) -> Self {
        counts.iter().fold(self, |points, count| {
            let mut attributes = extra();
            attributes.push(text(key, &count.name));
            points.point(attributes, count.count)
        })
    }

    fn gauge(
        self,
        name: &'static str,
        unit: &'static str,
        description: &'static str,
    ) -> Metric<'a> {
        Metric {
            name,
            description,
            unit,
            gauge: Gauge {
                data_points: self.points,
            },
        }
    }
}

/// Serialises a string-keyed enum value, e.g. a size bucket, to its wire
/// name.
fn wire_name(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("a unit enum serialises to a string"),
    }
}

/// The OTLP/HTTP JSON body for `report`.
///
/// Deterministic: every point is stamped with midnight UTC of
/// `report.generated_on`, never the moment of encoding, so the same report
/// always encodes to the same bytes.
pub fn encode_otlp_json(report: &LibraryReport) -> Vec<u8> {
    let LibraryReport {
        schema_version,
        server_version,
        generated_on,
        libraries,
        titles,
        seasons,
        episodes,
        files,
        containers,
        codecs,
        file_sizes,
        total_size,
    } = report;

    let midnight = Utc.from_utc_datetime(&generated_on.and_time(NaiveTime::MIN));
    let time_unix_nano = midnight
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_string();
    let at = time_unix_nano.as_str();
    let size_names: Vec<String> = file_sizes
        .iter()
        .map(|size| wire_name(&size.bucket))
        .collect();
    let total_size = wire_name(total_size);

    let metrics = vec![
        Points::new(at).point(Vec::new(), *libraries).gauge(
            "beam.library.libraries",
            "{library}",
            "Registered libraries.",
        ),
        Points::new(at)
            .point(vec![text("media_type", "movie")], titles.movies)
            .point(vec![text("media_type", "show")], titles.shows)
            .gauge(
                "beam.library.titles",
                "{title}",
                "Titles with at least one present file.",
            ),
        Points::new(at).point(Vec::new(), *seasons).gauge(
            "beam.library.seasons",
            "{season}",
            "Seasons with at least one present episode file.",
        ),
        Points::new(at).point(Vec::new(), *episodes).gauge(
            "beam.library.episodes",
            "{episode}",
            "Episodes with a present file.",
        ),
        Points::new(at)
            .point(vec![text("content_type", "movie")], files.movie)
            .point(vec![text("content_type", "episode")], files.episode)
            .point(
                vec![text("content_type", "unclassified")],
                files.unclassified,
            )
            .gauge(
                "beam.library.files",
                "{file}",
                "Present files by content type.",
            ),
        Points::new(at).named("container", containers, Vec::new).gauge(
            "beam.library.files.by_container",
            "{file}",
            "Present files by container.",
        ),
        file_sizes
            .iter()
            .zip(&size_names)
            .fold(Points::new(at), |points, (size, name)| {
                points.point(vec![text("size_bucket", name)], size.count)
            })
            .gauge(
                "beam.library.files.by_size",
                "{file}",
                "Present files by size range.",
            ),
        Points::new(at)
            .named("codec", &codecs.video, || vec![text("stream_type", "video")])
            .named("codec", &codecs.audio, || vec![text("stream_type", "audio")])
            .named("codec", &codecs.subtitle, || {
                vec![text("stream_type", "subtitle")]
            })
            .gauge(
                "beam.library.streams",
                "{stream}",
                "Streams of present files by type and codec.",
            ),
        Points::new(at)
            .point(vec![text("size_bucket", &total_size)], 1)
            .gauge(
                "beam.library.total_size",
                "1",
                "Always 1; the size_bucket attribute is the range the server's indexed bytes fall into.",
            ),
    ];

    let request = ExportMetricsServiceRequest {
        resource_metrics: [ResourceMetrics {
            resource: Resource {
                attributes: vec![
                    text("service.name", SERVICE_NAME),
                    text("service.version", server_version),
                    KeyValue {
                        key: "beam.report.schema_version",
                        value: AnyValue::Int(schema_version.to_string()),
                    },
                ],
            },
            scope_metrics: [ScopeMetrics {
                scope: Scope {
                    name: SCOPE_NAME,
                    version: schema_version.to_string(),
                },
                metrics,
            }],
        }],
    };

    serde_json::to_vec(&request).expect("the request is plain data and always serialises")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use beam_domain::models::library_shape::{FilesByContentType, LibraryShape, NamedCount};
    use beam_domain::utils::telemetry::{FileSizeHistogram, GIB};
    use chrono::NaiveDate;
    use serde_json::Value;

    use super::*;
    use crate::services::telemetry::library_report::build_library_report;

    fn report() -> LibraryReport {
        let mut file_sizes = FileSizeHistogram::default();
        for size in [1, GIB, 5 * GIB, 60 * GIB] {
            file_sizes.record(size);
        }
        build_library_report(
            &LibraryShape {
                libraries: 2,
                movies: 3,
                shows: 4,
                seasons: 5,
                episodes: 6,
                files: FilesByContentType {
                    movie: 7,
                    episode: 8,
                    unclassified: 9,
                },
                containers: vec![NamedCount::new("matroska,webm", 10)],
                video_codecs: vec![NamedCount::new("H264", 11)],
                audio_codecs: vec![NamedCount::new("aac", 12)],
                subtitle_codecs: vec![NamedCount::new("subrip", 13)],
                file_sizes,
                total_bytes: 66 * GIB,
            },
            "9.8.7",
            NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        )
    }

    /// `(metric name, sorted attributes) -> value`, decoded from the body.
    type Points = BTreeMap<(String, Vec<(String, String)>), u64>;

    /// Reads every gauge point back out of an encoded body, independently of
    /// the private structs that wrote it.
    fn decode(body: &[u8]) -> (Value, Points) {
        let json: Value = serde_json::from_slice(body).expect("the body is JSON");
        let mut points = Points::new();
        for metric in json["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .unwrap()
        {
            let name = metric["name"].as_str().unwrap().to_string();
            for point in metric["gauge"]["dataPoints"].as_array().unwrap() {
                let mut attributes: Vec<(String, String)> = point["attributes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|kv| {
                        (
                            kv["key"].as_str().unwrap().to_string(),
                            kv["value"]["stringValue"].as_str().unwrap().to_string(),
                        )
                    })
                    .collect();
                attributes.sort();
                let value: u64 = point["asInt"].as_str().unwrap().parse().unwrap();
                assert!(
                    points.insert((name.clone(), attributes), value).is_none(),
                    "two points of {name} share their attributes"
                );
            }
        }
        (json, points)
    }

    fn at(points: &Points, name: &str, attributes: &[(&str, &str)]) -> Option<u64> {
        let mut key: Vec<(String, String)> = attributes
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        key.sort();
        points.get(&(name.to_string(), key)).copied()
    }

    #[test]
    fn every_number_in_the_report_is_a_point() {
        let report = report();
        let (_, points) = decode(&encode_otlp_json(&report));

        assert_eq!(at(&points, "beam.library.libraries", &[]), Some(2));
        assert_eq!(
            at(&points, "beam.library.titles", &[("media_type", "movie")]),
            Some(3)
        );
        assert_eq!(
            at(&points, "beam.library.titles", &[("media_type", "show")]),
            Some(4)
        );
        assert_eq!(at(&points, "beam.library.seasons", &[]), Some(5));
        assert_eq!(at(&points, "beam.library.episodes", &[]), Some(6));
        for (content_type, count) in [("movie", 7), ("episode", 8), ("unclassified", 9)] {
            assert_eq!(
                at(
                    &points,
                    "beam.library.files",
                    &[("content_type", content_type)]
                ),
                Some(count)
            );
        }
        assert_eq!(
            at(
                &points,
                "beam.library.files.by_container",
                &[("container", "matroska,webm")]
            ),
            Some(10)
        );
        for (stream_type, codec, count) in [
            ("video", "h264", 11),
            ("audio", "aac", 12),
            ("subtitle", "subrip", 13),
        ] {
            assert_eq!(
                at(
                    &points,
                    "beam.library.streams",
                    &[("stream_type", stream_type), ("codec", codec)]
                ),
                Some(count)
            );
        }
        for size in &report.file_sizes {
            assert_eq!(
                at(
                    &points,
                    "beam.library.files.by_size",
                    &[("size_bucket", &wire_name(&size.bucket))]
                ),
                Some(size.count)
            );
        }
        assert_eq!(
            at(
                &points,
                "beam.library.total_size",
                &[("size_bucket", &wire_name(&report.total_size))]
            ),
            Some(1)
        );
        let expected_points = 1 + 2 + 1 + 1 + 3 + 1 + report.file_sizes.len() + 3 + 1;
        assert_eq!(points.len(), expected_points, "no point beyond the report");
    }

    #[test]
    fn the_resource_names_the_service_and_version_and_nothing_else() {
        let (json, _) = decode(&encode_otlp_json(&report()));
        let attributes = &json["resourceMetrics"][0]["resource"]["attributes"];

        let keys: Vec<&str> = attributes
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| kv["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            vec![
                "service.name",
                "service.version",
                "beam.report.schema_version"
            ],
            "no host name, instance id or anything else that identifies a server"
        );
        assert_eq!(attributes[1]["value"]["stringValue"], "9.8.7");
    }

    /// Every point is stamped midnight UTC of the report's day, so the send
    /// time is not in the payload.
    #[test]
    fn every_point_is_stamped_with_the_start_of_the_day() {
        let body = encode_otlp_json(&report());
        let json: Value = serde_json::from_slice(&body).unwrap();
        let midnight = "1790467200000000000"; // 2026-09-27T00:00:00Z

        for metric in json["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .unwrap()
        {
            for point in metric["gauge"]["dataPoints"].as_array().unwrap() {
                assert_eq!(point["timeUnixNano"], midnight, "{}", metric["name"]);
            }
        }
    }

    #[test]
    fn the_same_report_always_encodes_to_the_same_bytes() {
        assert_eq!(encode_otlp_json(&report()), encode_otlp_json(&report()));
    }

    /// A path in a label would be the leak this feature must never have.
    #[test]
    fn no_attribute_value_is_path_like() {
        let (_, points) = decode(&encode_otlp_json(&report()));
        for ((name, attributes), _) in points {
            for (key, value) in attributes {
                assert!(
                    !value.contains('/') && !value.contains('\\'),
                    "{name} {key}={value}"
                );
            }
        }
    }
}
