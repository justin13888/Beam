//! The anonymous library report (issue #93, ADR-0019), as an admin previews
//! it.
//!
//! Every field is an aggregate, and every number is a range. There is
//! deliberately nothing here that could name a title, a path, a user or this
//! server: no identifier of any kind, and no exact count. That is not a promise
//! that two reports cannot be linked -- a distinctive enough shape, held
//! steady from week to week, may still let a collector correlate them
//! (ADR-0019).

use beam_domain::utils::telemetry::{CountBucket, FileSizeBucket, TotalSizeBucket};
use chrono::{DateTime, NaiveDate, Utc};
use kynos::Schema;
use serde::Serialize;

/// The version of this report's shape. Raised whenever a field is added,
/// removed or changes meaning, so a collector can tell reports apart.
pub const LIBRARY_REPORT_SCHEMA_VERSION: u32 = 1;

/// The order of magnitude a count falls into. A report never carries an exact
/// count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Schema)]
pub enum LibraryCountBucket {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "from_1_to_9")]
    From1To9,
    #[serde(rename = "from_10_to_99")]
    From10To99,
    #[serde(rename = "from_100_to_999")]
    From100To999,
    #[serde(rename = "from_1000_to_9999")]
    From1000To9999,
    #[serde(rename = "at_least_10000")]
    AtLeast10000,
}

impl From<CountBucket> for LibraryCountBucket {
    fn from(bucket: CountBucket) -> Self {
        match bucket {
            CountBucket::None => Self::None,
            CountBucket::From1To9 => Self::From1To9,
            CountBucket::From10To99 => Self::From10To99,
            CountBucket::From100To999 => Self::From100To999,
            CountBucket::From1000To9999 => Self::From1000To9999,
            CountBucket::AtLeast10000 => Self::AtLeast10000,
        }
    }
}

impl LibraryCountBucket {
    /// The bucket `count` falls into.
    pub fn of(count: u64) -> Self {
        CountBucket::of(count).into()
    }
}

/// How many of one named thing a library holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Schema)]
pub struct LibraryReportCount {
    /// A container or codec name, lowercased, as FFmpeg names it; `unknown`
    /// when the prober could not name it.
    pub name: String,
    pub count: LibraryCountBucket,
}

/// Live titles by kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct LibraryReportTitles {
    pub movies: LibraryCountBucket,
    pub shows: LibraryCountBucket,
}

/// Present files by what the indexer classified them as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct LibraryReportFiles {
    pub movie: LibraryCountBucket,
    pub episode: LibraryCountBucket,
    pub unclassified: LibraryCountBucket,
}

/// Streams of present files, per codec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct LibraryReportCodecs {
    pub video: Vec<LibraryReportCount>,
    pub audio: Vec<LibraryReportCount>,
    pub subtitle: Vec<LibraryReportCount>,
}

/// The size range one file falls into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Schema)]
pub enum LibraryFileSizeBucket {
    #[serde(rename = "under_1_gib")]
    Under1Gib,
    #[serde(rename = "from_1_to_4_gib")]
    From1To4Gib,
    #[serde(rename = "from_4_to_10_gib")]
    From4To10Gib,
    #[serde(rename = "from_10_to_25_gib")]
    From10To25Gib,
    #[serde(rename = "from_25_to_50_gib")]
    From25To50Gib,
    #[serde(rename = "at_least_50_gib")]
    AtLeast50Gib,
}

impl From<FileSizeBucket> for LibraryFileSizeBucket {
    fn from(bucket: FileSizeBucket) -> Self {
        match bucket {
            FileSizeBucket::Under1Gib => Self::Under1Gib,
            FileSizeBucket::From1To4Gib => Self::From1To4Gib,
            FileSizeBucket::From4To10Gib => Self::From4To10Gib,
            FileSizeBucket::From10To25Gib => Self::From10To25Gib,
            FileSizeBucket::From25To50Gib => Self::From25To50Gib,
            FileSizeBucket::AtLeast50Gib => Self::AtLeast50Gib,
        }
    }
}

/// How many present files fall into one size range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct LibraryReportFileSize {
    pub bucket: LibraryFileSizeBucket,
    pub count: LibraryCountBucket,
}

/// The range the server's indexed bytes fall into. Never reported exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Schema)]
pub enum LibraryTotalSizeBucket {
    #[serde(rename = "under_100_gib")]
    Under100Gib,
    #[serde(rename = "from_100_gib_to_1_tib")]
    From100GibTo1Tib,
    #[serde(rename = "from_1_to_10_tib")]
    From1To10Tib,
    #[serde(rename = "from_10_to_50_tib")]
    From10To50Tib,
    #[serde(rename = "at_least_50_tib")]
    AtLeast50Tib,
}

impl From<TotalSizeBucket> for LibraryTotalSizeBucket {
    fn from(bucket: TotalSizeBucket) -> Self {
        match bucket {
            TotalSizeBucket::Under100Gib => Self::Under100Gib,
            TotalSizeBucket::From100GibTo1Tib => Self::From100GibTo1Tib,
            TotalSizeBucket::From1To10Tib => Self::From1To10Tib,
            TotalSizeBucket::From10To50Tib => Self::From10To50Tib,
            TotalSizeBucket::AtLeast50Tib => Self::AtLeast50Tib,
        }
    }
}

/// The anonymous library report: what a server holds, in aggregate, every
/// number as a range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct LibraryReport {
    /// This report's shape; see the operator documentation.
    pub schema_version: u32,
    /// The Beam release that produced the report.
    pub server_version: String,
    /// The UTC day the report describes. A day rather than an instant, so the
    /// moment of sending says nothing about when the server is running.
    pub generated_on: NaiveDate,
    pub libraries: LibraryCountBucket,
    pub titles: LibraryReportTitles,
    /// Seasons with at least one present episode file.
    pub seasons: LibraryCountBucket,
    /// Episodes with a present file.
    pub episodes: LibraryCountBucket,
    pub files: LibraryReportFiles,
    /// Present files per container, sorted by name.
    pub containers: Vec<LibraryReportCount>,
    pub codecs: LibraryReportCodecs,
    /// Present files per size range, smallest first; every range is listed.
    pub file_sizes: Vec<LibraryReportFileSize>,
    pub total_size: LibraryTotalSizeBucket,
}

/// What `GET /v1/admin/telemetry/library` answers: the report this server
/// would send now, byte for byte, and whether and where it would go.
#[derive(Clone, Debug, Serialize, Schema)]
pub struct LibraryTelemetryPreview {
    /// Whether `BEAM_TELEMETRY_URL` is set. When false nothing is ever sent;
    /// the preview is still available, to decide whether to opt in.
    pub destination_configured: bool,
    /// `scheme://host[:port]` of the collector; its path and query are not
    /// shown, since they may carry an ingest token.
    pub destination_origin: Option<String>,
    /// When a report last reached the collector.
    pub last_sent_at: Option<DateTime<Utc>>,
    /// When the next report is due, once the schedule is running.
    pub next_send_at: Option<DateTime<Utc>>,
    pub report: LibraryReport,
    /// The `Content-Type` the payload is sent under.
    pub content_type: String,
    /// The exact request body that would be sent: the report encoded as
    /// OTLP/HTTP JSON metrics.
    pub payload: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collector groups by these strings, so the wire spelling must be the
    /// one the domain -- and so the OTLP attribute -- uses. Derived from the
    /// domain's list, not restated.
    #[test]
    fn wire_bucket_names_are_the_domain_names() {
        for bucket in CountBucket::ALL {
            assert_eq!(
                serde_json::to_value(LibraryCountBucket::from(bucket)).unwrap(),
                bucket.as_str()
            );
        }
        for bucket in FileSizeBucket::ALL {
            assert_eq!(
                serde_json::to_value(LibraryFileSizeBucket::from(bucket)).unwrap(),
                bucket.as_str()
            );
        }
        for bucket in TotalSizeBucket::ALL {
            assert_eq!(
                serde_json::to_value(LibraryTotalSizeBucket::from(bucket)).unwrap(),
                bucket.as_str()
            );
        }
    }
}
