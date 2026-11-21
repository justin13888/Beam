//! The anonymous library telemetry report an administrator previews before
//! opting in (#93): every number a range, never a count.

use crate::api::types as wire;

/// The order of magnitude a count falls into. The report never carries an
/// exact count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CountRange {
    /// Nothing.
    None,
    /// 1 to 9.
    From1To9,
    /// 10 to 99.
    From10To99,
    /// 100 to 999.
    From100To999,
    /// 1,000 to 9,999.
    From1000To9999,
    /// 10,000 or more.
    AtLeast10000,
}

/// The size range one file falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FileSizeRange {
    /// Under 1 GiB.
    Under1Gib,
    /// 1 to 4 GiB.
    From1To4Gib,
    /// 4 to 10 GiB.
    From4To10Gib,
    /// 10 to 25 GiB.
    From10To25Gib,
    /// 25 to 50 GiB.
    From25To50Gib,
    /// 50 GiB or more.
    AtLeast50Gib,
}

/// The range the server's indexed bytes fall into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TotalSizeRange {
    /// Under 100 GiB.
    Under100Gib,
    /// 100 GiB to 1 TiB.
    From100GibTo1Tib,
    /// 1 to 10 TiB.
    From1To10Tib,
    /// 10 to 50 TiB.
    From10To50Tib,
    /// 50 TiB or more.
    AtLeast50Tib,
}

/// How many of one named thing -- a container or a codec -- a library holds.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NamedCount {
    /// The name, lowercased as FFmpeg names it; `unknown` where the prober
    /// could not name it.
    pub name: String,
    /// How many, as a range.
    pub count: CountRange,
}

/// How many present files fall into one size range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FileSizeCount {
    /// The size range.
    pub range: FileSizeRange,
    /// How many files fall into it, as a range.
    pub count: CountRange,
}

/// The anonymous library report: what a server holds, in aggregate.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LibraryReport {
    /// The report's shape.
    pub schema_version: u32,
    /// The Beam release that produced it.
    pub server_version: String,
    /// The UTC day it describes, `YYYY-MM-DD`.
    pub generated_on: String,
    /// Libraries configured.
    pub libraries: CountRange,
    /// Live movies.
    pub movies: CountRange,
    /// Live shows.
    pub shows: CountRange,
    /// Seasons with at least one present episode file.
    pub seasons: CountRange,
    /// Episodes with a present file.
    pub episodes: CountRange,
    /// Present files classified as movies.
    pub movie_files: CountRange,
    /// Present files classified as episodes.
    pub episode_files: CountRange,
    /// Present files the indexer could not classify.
    pub unclassified_files: CountRange,
    /// Present files per container, sorted by name.
    pub containers: Vec<NamedCount>,
    /// Video streams per codec.
    pub video_codecs: Vec<NamedCount>,
    /// Audio streams per codec.
    pub audio_codecs: Vec<NamedCount>,
    /// Subtitle streams per codec.
    pub subtitle_codecs: Vec<NamedCount>,
    /// Present files per size range, smallest first; every range is listed.
    pub file_sizes: Vec<FileSizeCount>,
    /// The range the indexed bytes fall into.
    pub total_size: TotalSizeRange,
}

/// The report this server would send now, byte for byte, and whether and
/// where it would go.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LibraryTelemetryPreview {
    /// Whether a collector is configured. When false nothing is ever sent.
    pub destination_configured: bool,
    /// `scheme://host[:port]` of the collector.
    pub destination_origin: Option<String>,
    /// When a report last reached the collector, as a Unix timestamp.
    pub last_sent_at_unix: Option<i64>,
    /// When the next report is due, as a Unix timestamp.
    pub next_send_at_unix: Option<i64>,
    /// The report, as structured values.
    pub report: LibraryReport,
    /// The `Content-Type` the payload is sent under.
    pub content_type: String,
    /// The exact request body that would be sent.
    pub payload: String,
}

impl CountRange {
    fn from_wire(bucket: wire::LibraryCountBucket) -> Self {
        match bucket {
            wire::LibraryCountBucket::None => Self::None,
            wire::LibraryCountBucket::From1To9 => Self::From1To9,
            wire::LibraryCountBucket::From10To99 => Self::From10To99,
            wire::LibraryCountBucket::From100To999 => Self::From100To999,
            wire::LibraryCountBucket::From1000To9999 => Self::From1000To9999,
            wire::LibraryCountBucket::AtLeast10000 => Self::AtLeast10000,
        }
    }
}

impl FileSizeRange {
    fn from_wire(bucket: wire::LibraryFileSizeBucket) -> Self {
        match bucket {
            wire::LibraryFileSizeBucket::Under1Gib => Self::Under1Gib,
            wire::LibraryFileSizeBucket::From1To4Gib => Self::From1To4Gib,
            wire::LibraryFileSizeBucket::From4To10Gib => Self::From4To10Gib,
            wire::LibraryFileSizeBucket::From10To25Gib => Self::From10To25Gib,
            wire::LibraryFileSizeBucket::From25To50Gib => Self::From25To50Gib,
            wire::LibraryFileSizeBucket::AtLeast50Gib => Self::AtLeast50Gib,
        }
    }
}

impl TotalSizeRange {
    fn from_wire(bucket: wire::LibraryTotalSizeBucket) -> Self {
        match bucket {
            wire::LibraryTotalSizeBucket::Under100Gib => Self::Under100Gib,
            wire::LibraryTotalSizeBucket::From100GibTo1Tib => Self::From100GibTo1Tib,
            wire::LibraryTotalSizeBucket::From1To10Tib => Self::From1To10Tib,
            wire::LibraryTotalSizeBucket::From10To50Tib => Self::From10To50Tib,
            wire::LibraryTotalSizeBucket::AtLeast50Tib => Self::AtLeast50Tib,
        }
    }
}

fn named_counts(counts: Vec<wire::LibraryReportCount>) -> Vec<NamedCount> {
    counts
        .into_iter()
        .map(|wire::LibraryReportCount { name, count }| NamedCount {
            name,
            count: CountRange::from_wire(count),
        })
        .collect()
}

impl LibraryReport {
    /// Normalise a generated library report.
    #[must_use]
    pub fn from_generated(report: wire::LibraryReport) -> Self {
        let wire::LibraryReport {
            codecs,
            containers,
            episodes,
            file_sizes,
            files,
            generated_on,
            libraries,
            schema_version,
            seasons,
            server_version,
            titles,
            total_size,
        } = report;
        let wire::LibraryReportTitles { movies, shows } = titles;
        let wire::LibraryReportFiles {
            episode,
            movie,
            unclassified,
        } = files;
        let wire::LibraryReportCodecs {
            audio,
            subtitle,
            video,
        } = codecs;
        Self {
            // A shape number the contract bounds to `uint32`; a negative one
            // is none the server could have meant.
            schema_version: u32::try_from(schema_version).unwrap_or(0),
            server_version,
            generated_on: generated_on.0.to_string(),
            libraries: CountRange::from_wire(libraries),
            movies: CountRange::from_wire(movies),
            shows: CountRange::from_wire(shows),
            seasons: CountRange::from_wire(seasons),
            episodes: CountRange::from_wire(episodes),
            movie_files: CountRange::from_wire(movie),
            episode_files: CountRange::from_wire(episode),
            unclassified_files: CountRange::from_wire(unclassified),
            containers: named_counts(containers),
            video_codecs: named_counts(video),
            audio_codecs: named_counts(audio),
            subtitle_codecs: named_counts(subtitle),
            file_sizes: file_sizes
                .into_iter()
                .map(
                    |wire::LibraryReportFileSize { bucket, count }| FileSizeCount {
                        range: FileSizeRange::from_wire(bucket),
                        count: CountRange::from_wire(count),
                    },
                )
                .collect(),
            total_size: TotalSizeRange::from_wire(total_size),
        }
    }
}

impl LibraryTelemetryPreview {
    /// Normalise a generated telemetry preview.
    #[must_use]
    pub fn from_generated(preview: wire::LibraryTelemetryPreview) -> Self {
        let wire::LibraryTelemetryPreview {
            content_type,
            destination_configured,
            destination_origin,
            last_sent_at,
            next_send_at,
            payload,
            report,
        } = preview;
        Self {
            destination_configured,
            destination_origin,
            last_sent_at_unix: last_sent_at.map(|at| at.0.unix_timestamp()),
            next_send_at_unix: next_send_at.map(|at| at.0.unix_timestamp()),
            report: LibraryReport::from_generated(report),
            content_type,
            payload,
        }
    }
}

/// A preview the server could send, shared with the FFI tests.
#[cfg(test)]
pub(crate) const PREVIEW_JSON: &str = r#"{"destination_configured":true,
    "destination_origin":"https://collector.example",
    "last_sent_at":null,"next_send_at":"2026-01-02T00:00:00Z",
    "content_type":"application/json","payload":"{\"resourceMetrics\":[]}",
    "report":{"schema_version":1,"server_version":"1.2.3","generated_on":"2026-01-01",
      "libraries":"from_1_to_9",
      "titles":{"movies":"from_100_to_999","shows":"from_10_to_99"},
      "seasons":"from_10_to_99","episodes":"from_1000_to_9999",
      "files":{"movie":"from_100_to_999","episode":"from_1000_to_9999","unclassified":"none"},
      "containers":[{"name":"matroska","count":"from_1000_to_9999"}],
      "codecs":{"video":[{"name":"hevc","count":"from_100_to_999"}],
                "audio":[{"name":"aac","count":"at_least_10000"}],"subtitle":[]},
      "file_sizes":[{"bucket":"under_1_gib","count":"from_1_to_9"},
                    {"bucket":"at_least_50_gib","count":"none"}],
      "total_size":"from_1_to_10_tib"}}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_carries_its_destination_payload_and_every_range() {
        let preview = LibraryTelemetryPreview::from_generated(
            serde_json::from_str(PREVIEW_JSON).expect("a valid preview"),
        );

        assert!(preview.destination_configured);
        assert_eq!(
            preview.destination_origin.as_deref(),
            Some("https://collector.example")
        );
        assert_eq!(preview.last_sent_at_unix, None);
        assert_eq!(preview.next_send_at_unix, Some(1_767_312_000));
        assert_eq!(preview.payload, r#"{"resourceMetrics":[]}"#);
        let report = preview.report;
        assert_eq!(report.generated_on, "2026-01-01");
        assert_eq!(
            (report.movies, report.shows, report.episodes),
            (
                CountRange::From100To999,
                CountRange::From10To99,
                CountRange::From1000To9999
            )
        );
        assert_eq!(
            (report.movie_files, report.unclassified_files),
            (CountRange::From100To999, CountRange::None)
        );
        assert_eq!(
            report.audio_codecs,
            vec![NamedCount {
                name: "aac".to_owned(),
                count: CountRange::AtLeast10000,
            }]
        );
        assert!(report.subtitle_codecs.is_empty());
        assert_eq!(
            report.file_sizes,
            vec![
                FileSizeCount {
                    range: FileSizeRange::Under1Gib,
                    count: CountRange::From1To9,
                },
                FileSizeCount {
                    range: FileSizeRange::AtLeast50Gib,
                    count: CountRange::None,
                },
            ]
        );
        assert_eq!(report.total_size, TotalSizeRange::From1To10Tib);
    }

    /// Each range enum folds the contract's declared buckets one to one:
    /// no two buckets read as the same range. Read from the document.
    #[test]
    fn every_declared_bucket_maps_to_its_own_range() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../api/openapi.json")).expect("the document");
        let declared = |schema: &str| -> Vec<serde_json::Value> {
            document
                .pointer(&format!("/components/schemas/{schema}/enum"))
                .and_then(serde_json::Value::as_array)
                .expect("the contract declares the buckets")
                .clone()
        };
        let distinct = |names: Vec<String>| {
            let total = names.len();
            let unique: std::collections::HashSet<String> = names.into_iter().collect();
            assert!(total > 0 && unique.len() == total);
        };
        distinct(
            declared("LibraryCountBucket")
                .into_iter()
                .map(|value| {
                    format!(
                        "{:?}",
                        CountRange::from_wire(serde_json::from_value(value).expect("a bucket"))
                    )
                })
                .collect(),
        );
        distinct(
            declared("LibraryFileSizeBucket")
                .into_iter()
                .map(|value| {
                    format!(
                        "{:?}",
                        FileSizeRange::from_wire(serde_json::from_value(value).expect("a bucket"))
                    )
                })
                .collect(),
        );
        distinct(
            declared("LibraryTotalSizeBucket")
                .into_iter()
                .map(|value| {
                    format!(
                        "{:?}",
                        TotalSizeRange::from_wire(serde_json::from_value(value).expect("a bucket"))
                    )
                })
                .collect(),
        );
    }
}
