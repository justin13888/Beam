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
//!
//! The report has no exact number to give, so no point carries one: every
//! point's value is `1`, and its range is an attribute -- `count_bucket` for a
//! count, `size_bucket` for the total size. A collector that keeps each
//! request as it arrives counts servers per range by summing the `1`s; an OTLP
//! backend may instead merge identical series (ADR-0019, Consequences).

use std::borrow::Cow;

use chrono::{NaiveTime, TimeZone, Utc};
use serde::Serialize;

use crate::models::telemetry::{LibraryCountBucket, LibraryReport, LibraryReportCount};

/// The `Content-Type` of an OTLP/HTTP JSON request.
pub const OTLP_JSON_CONTENT_TYPE: &str = "application/json";

/// `service.name` on every report.
const SERVICE_NAME: &str = "beam-server";

/// The instrumentation scope the gauges belong to.
const SCOPE_NAME: &str = "beam.library_report";

/// The attribute a point's count range is carried in.
const COUNT_BUCKET: &str = "count_bucket";

/// The unit of every gauge: each point is the dimensionless `1` of "this
/// server is in this range".
const UNIT: &str = "1";

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
    String(Cow<'a, str>),
    #[serde(rename = "intValue")]
    Int(String),
}

fn text<'a>(key: &'static str, value: impl Into<Cow<'a, str>>) -> KeyValue<'a> {
    KeyValue {
        key,
        value: AnyValue::String(value.into()),
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

    /// A point meaning "in this range": the value is always `1`, and the
    /// range is among `attributes`.
    fn point(mut self, attributes: Vec<KeyValue<'a>>) -> Self {
        self.points.push(NumberDataPoint {
            attributes,
            time_unix_nano: self.time_unix_nano.to_string(),
            as_int: "1".to_string(),
        });
        self
    }

    /// A point for a count, its range in `count_bucket`.
    fn counted(self, mut attributes: Vec<KeyValue<'a>>, count: LibraryCountBucket) -> Self {
        attributes.push(text(COUNT_BUCKET, wire_name(&count)));
        self.point(attributes)
    }

    fn named(
        self,
        key: &'static str,
        counts: &'a [LibraryReportCount],
        extra: impl Fn() -> Vec<KeyValue<'a>>,
    ) -> Self {
        counts.iter().fold(self, |points, count| {
            let mut attributes = extra();
            attributes.push(text(key, count.name.as_str()));
            points.counted(attributes, count.count)
        })
    }

    fn gauge(self, name: &'static str, description: &'static str) -> Metric<'a> {
        Metric {
            name,
            description,
            unit: UNIT,
            gauge: Gauge {
                data_points: self.points,
            },
        }
    }
}

/// Serialises a string-keyed enum value, e.g. a size or count bucket, to its
/// wire name.
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

    let metrics = vec![
        Points::new(at).counted(Vec::new(), *libraries).gauge(
            "beam.library.libraries",
            "Registered libraries, as a range.",
        ),
        Points::new(at)
            .counted(vec![text("media_type", "movie")], titles.movies)
            .counted(vec![text("media_type", "show")], titles.shows)
            .gauge(
                "beam.library.titles",
                "Titles with at least one present file, as a range.",
            ),
        Points::new(at).counted(Vec::new(), *seasons).gauge(
            "beam.library.seasons",
            "Seasons with at least one present episode file, as a range.",
        ),
        Points::new(at).counted(Vec::new(), *episodes).gauge(
            "beam.library.episodes",
            "Episodes with a present file, as a range.",
        ),
        Points::new(at)
            .counted(vec![text("content_type", "movie")], files.movie)
            .counted(vec![text("content_type", "episode")], files.episode)
            .counted(
                vec![text("content_type", "unclassified")],
                files.unclassified,
            )
            .gauge(
                "beam.library.files",
                "Present files by content type, as a range.",
            ),
        Points::new(at)
            .named("container", containers, Vec::new)
            .gauge(
                "beam.library.files.by_container",
                "Present files by container, as a range.",
            ),
        file_sizes
            .iter()
            .fold(Points::new(at), |points, size| {
                points.counted(
                    vec![text("size_bucket", wire_name(&size.bucket))],
                    size.count,
                )
            })
            .gauge(
                "beam.library.files.by_size",
                "Present files by size range, as a range.",
            ),
        Points::new(at)
            .named("codec", &codecs.video, || {
                vec![text("stream_type", "video")]
            })
            .named("codec", &codecs.audio, || {
                vec![text("stream_type", "audio")]
            })
            .named("codec", &codecs.subtitle, || {
                vec![text("stream_type", "subtitle")]
            })
            .gauge(
                "beam.library.streams",
                "Streams of present files by type and codec, as a range.",
            ),
        Points::new(at)
            .point(vec![text("size_bucket", wire_name(total_size))])
            .gauge(
                "beam.library.total_size",
                "The range the server's indexed bytes fall into.",
            ),
    ];

    let request = ExportMetricsServiceRequest {
        resource_metrics: [ResourceMetrics {
            resource: Resource {
                attributes: vec![
                    text("service.name", SERVICE_NAME),
                    text("service.version", server_version.as_str()),
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

    /// Counts spread over every count range, so a point found under the
    /// wrong range's attribute cannot pass for the right one.
    fn report() -> LibraryReport {
        let mut file_sizes = FileSizeHistogram::default();
        for size in [1, GIB, 5 * GIB, 60 * GIB] {
            file_sizes.record(size);
        }
        build_library_report(
            &LibraryShape {
                libraries: 2,
                movies: 30,
                shows: 4,
                seasons: 0,
                episodes: 600,
                files: FilesByContentType {
                    movie: 70,
                    episode: 8_000,
                    unclassified: 90_000,
                },
                containers: vec![
                    NamedCount::new("matroska,webm", 10),
                    NamedCount::new("mov,mp4,m4a,3gp,3g2,mj2", 1),
                ],
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

    /// Every range in the report is one point, found by its attributes and
    /// its `count_bucket`, and every point's value is the `1` of "in this
    /// range" -- never a count.
    #[test]
    fn every_range_in_the_report_is_a_point_of_one() {
        let report = report();
        let (_, points) = decode(&encode_otlp_json(&report));
        let bucket = |count: &LibraryCountBucket| wire_name(count);

        assert!(
            points.values().all(|value| *value == 1),
            "no exact number: {points:?}"
        );
        let has = |name: &str, attributes: &[(&str, &str)]| {
            assert_eq!(
                at(&points, name, attributes),
                Some(1),
                "{name} {attributes:?}"
            );
        };
        has(
            "beam.library.libraries",
            &[("count_bucket", &bucket(&report.libraries))],
        );
        has(
            "beam.library.titles",
            &[
                ("media_type", "movie"),
                ("count_bucket", &bucket(&report.titles.movies)),
            ],
        );
        has(
            "beam.library.titles",
            &[
                ("media_type", "show"),
                ("count_bucket", &bucket(&report.titles.shows)),
            ],
        );
        has(
            "beam.library.seasons",
            &[("count_bucket", &bucket(&report.seasons))],
        );
        has(
            "beam.library.episodes",
            &[("count_bucket", &bucket(&report.episodes))],
        );
        for (content_type, count) in [
            ("movie", &report.files.movie),
            ("episode", &report.files.episode),
            ("unclassified", &report.files.unclassified),
        ] {
            has(
                "beam.library.files",
                &[
                    ("content_type", content_type),
                    ("count_bucket", &bucket(count)),
                ],
            );
        }
        for container in &report.containers {
            has(
                "beam.library.files.by_container",
                &[
                    ("container", &container.name),
                    ("count_bucket", &bucket(&container.count)),
                ],
            );
        }
        for (stream_type, counts) in [
            ("video", &report.codecs.video),
            ("audio", &report.codecs.audio),
            ("subtitle", &report.codecs.subtitle),
        ] {
            for codec in counts {
                has(
                    "beam.library.streams",
                    &[
                        ("stream_type", stream_type),
                        ("codec", &codec.name),
                        ("count_bucket", &bucket(&codec.count)),
                    ],
                );
            }
        }
        for size in &report.file_sizes {
            has(
                "beam.library.files.by_size",
                &[
                    ("size_bucket", &wire_name(&size.bucket)),
                    ("count_bucket", &bucket(&size.count)),
                ],
            );
        }
        has(
            "beam.library.total_size",
            &[("size_bucket", &wire_name(&report.total_size))],
        );
        let expected_points = 1
            + 2
            + 1
            + 1
            + 3
            + report.containers.len()
            + report.file_sizes.len()
            + report.codecs.video.len()
            + report.codecs.audio.len()
            + report.codecs.subtitle.len()
            + 1;
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
