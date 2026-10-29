//! Libraries, the files indexed into them, and their scans.

use crate::api::types as wire;

/// A library as the libraries screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LibrarySummary {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Operator-supplied description.
    pub description: Option<String>,
    /// Number of indexed files.
    pub size: u32,
    /// Files seen by the most recent scan.
    pub last_scan_file_count: Option<u32>,
    /// When the most recent scan started, as a Unix timestamp.
    pub last_scan_started_at_unix: Option<i64>,
    /// When the most recent scan finished, as a Unix timestamp.
    ///
    /// A started time with no finished time is a scan still running, which is
    /// what the libraries screen shows a progress indicator for.
    pub last_scan_finished_at_unix: Option<i64>,
}

/// What an indexed file was classified as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FileContentType {
    /// Matched to a film.
    Movie,
    /// Matched to an episode.
    Episode,
    /// Indexed but not matched to any title.
    Unclassified,
}

/// Whether the indexer's record of a file is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FileIndexStatus {
    /// Indexed and unchanged since.
    Known,
    /// Changed on disk since it was indexed.
    Changed,
    /// Seen but not yet probed.
    Unknown,
}

/// One indexed file, as the library detail screen lists it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LibraryFileSummary {
    /// Stable identifier.
    pub id: String,
    /// The library that owns it.
    pub library_id: String,
    /// Path relative to the library root, '/'-separated. Never absolute.
    pub path: String,
    /// Size on disk.
    pub size_bytes: u64,
    /// Container format as probed.
    pub container_format: Option<String>,
    /// MIME type as recorded.
    pub mime_type: Option<String>,
    /// Duration in seconds, where probing determined one.
    pub duration_secs: Option<f64>,
    /// What it was matched to.
    pub content_type: FileContentType,
    /// Whether the record is current.
    pub status: FileIndexStatus,
    /// When it was last scanned, as a Unix timestamp.
    pub scanned_at_unix: i64,
}

/// Where a library scan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ScanJobState {
    /// Accepted, and waiting for the library's turn.
    Queued,
    /// Walking and indexing the library now.
    Running,
    /// Finished.
    Succeeded,
    /// Stopped short; `failure` says why.
    Failed,
}

/// The latest scan of one library since the server started.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ScanJob {
    /// The scan's identifier.
    pub id: String,
    /// The library it scans.
    pub library_id: String,
    /// Where it is.
    pub state: ScanJobState,
    /// Files the walk found to index; `None` until the walk has finished.
    pub total_count: Option<u64>,
    /// Files handled so far, whatever became of them.
    pub processed_count: u64,
    /// Files indexed for the first time.
    pub added_count: u64,
    /// Indexed files whose content changed.
    pub changed_count: u64,
    /// Files that could not be processed.
    pub failed_count: u64,
    /// Files no longer on disk, newly marked missing.
    pub missing_count: u64,
    /// When it finished, as a Unix timestamp, once it has.
    pub finished_at_unix: Option<i64>,
    /// Why a failed scan failed. Names no filesystem path.
    pub failure: Option<String>,
}

impl LibrarySummary {
    /// Normalise a generated library.
    #[must_use]
    pub fn from_generated(library: wire::Library) -> Self {
        let wire::Library {
            description,
            file_count,
            id,
            last_scan_file_count,
            last_scan_finished_at,
            last_scan_started_at,
            name,
        } = library;
        Self {
            id: id.to_string(),
            name,
            description,
            // The record keeps its `size` name and `u32` width (issue #190
            // left the UniFFI surface alone); a count past it saturates.
            size: u32::try_from(file_count).unwrap_or(u32::MAX),
            last_scan_file_count: last_scan_file_count.and_then(|count| u32::try_from(count).ok()),
            last_scan_started_at_unix: last_scan_started_at.map(|at| at.0.unix_timestamp()),
            last_scan_finished_at_unix: last_scan_finished_at.map(|at| at.0.unix_timestamp()),
        }
    }
}

impl ScanJob {
    /// Normalise a generated scan job.
    #[must_use]
    pub fn from_generated(job: wire::ScanJob) -> Self {
        let wire::ScanJob {
            id,
            library_id,
            // Who started it is not something the operator acts on.
            trigger: _,
            state,
            queued_at: _,
            started_at: _,
            finished_at,
            progress,
            failure,
        } = job;
        let count = |value: i64| u64::try_from(value).unwrap_or(0);
        Self {
            id: id.to_string(),
            library_id: library_id.to_string(),
            state: match state {
                wire::ScanState::Queued => ScanJobState::Queued,
                wire::ScanState::Running => ScanJobState::Running,
                wire::ScanState::Succeeded => ScanJobState::Succeeded,
                wire::ScanState::Failed => ScanJobState::Failed,
            },
            total_count: progress
                .total_count
                .and_then(|total| u64::try_from(total).ok()),
            processed_count: count(progress.processed_count),
            added_count: count(progress.added_count),
            changed_count: count(progress.changed_count),
            failed_count: count(progress.failed_count),
            missing_count: count(progress.marked_missing_count),
            finished_at_unix: finished_at.map(|at| at.0.unix_timestamp()),
            failure,
        }
    }
}

impl LibraryFileSummary {
    /// Normalise a generated library file.
    #[must_use]
    pub fn from_generated(file: wire::LibraryFile) -> Self {
        let wire::LibraryFile {
            container_format,
            content_type,
            duration_secs,
            hash: _,
            id,
            library_id,
            mime_type,
            path,
            scanned_at,
            size_bytes,
            status,
            updated_at: _,
        } = file;
        Self {
            id: id.to_string(),
            library_id: library_id.to_string(),
            path,
            size_bytes: u64::try_from(size_bytes).unwrap_or(0),
            container_format,
            mime_type,
            duration_secs,
            content_type: match content_type {
                wire::FileContentType::Movie => FileContentType::Movie,
                wire::FileContentType::Episode => FileContentType::Episode,
                wire::FileContentType::Unclassified => FileContentType::Unclassified,
            },
            status: match status {
                wire::FileIndexStatus::Known => FileIndexStatus::Known,
                wire::FileIndexStatus::Changed => FileIndexStatus::Changed,
                wire::FileIndexStatus::Unknown => FileIndexStatus::Unknown,
            },
            scanned_at_unix: scanned_at.0.unix_timestamp(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_reports_a_running_scan_as_started_but_unfinished() {
        let json = r#"{"id":"00000000-0000-0000-0000-0000000000a1","name":"Films","file_count":42,
                       "last_scan_started_at":"2026-01-01T00:00:00Z"}"#;
        let library: wire::Library = serde_json::from_str(json).expect("a valid library");
        let summary = LibrarySummary::from_generated(library);
        assert_eq!(summary.size, 42);
        assert_eq!(summary.last_scan_started_at_unix, Some(1_767_225_600));
        assert_eq!(summary.last_scan_finished_at_unix, None);
    }
}
