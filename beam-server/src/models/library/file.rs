use std::path::{Component, Path};

use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::Serialize;
use tracing::warn;

/// File indexing status
#[derive(Clone, Copy, Debug, Serialize, Schema, Eq, PartialEq)]
pub enum FileIndexStatus {
    /// File is indexed and metadata matches
    Known,
    /// File exists but metadata/hash has changed since last scan
    Changed,
    /// File exists but extension is unknown/unsupported
    Unknown,
}

impl From<beam_domain::models::file::FileStatus> for FileIndexStatus {
    fn from(status: beam_domain::models::file::FileStatus) -> Self {
        match status {
            beam_domain::models::file::FileStatus::Known => FileIndexStatus::Known,
            beam_domain::models::file::FileStatus::Changed => FileIndexStatus::Changed,
            beam_domain::models::file::FileStatus::Unknown => FileIndexStatus::Unknown,
        }
    }
}

/// The kind of content a media file represents
#[derive(Clone, Copy, Debug, Serialize, Schema, Eq, PartialEq)]
pub enum FileContentType {
    /// File is associated with a movie
    Movie,
    /// File is associated with a TV episode
    Episode,
    /// Content type is not yet determined
    Unclassified,
}

/// A media file within a library
#[derive(Clone, Debug, Serialize, Schema)]
pub struct LibraryFile {
    pub id: String,
    pub library_id: String,
    /// Path of the file relative to its library's root, '/'-separated. Never
    /// absolute (NFR-108).
    pub path: String,
    /// File size in bytes
    pub size_bytes: i64,
    /// Content hash (XXH3) as a decimal string. Identifies the file's content
    /// for caching and duplicate detection.
    pub hash: String,
    /// MIME type (e.g. "video/mp4")
    pub mime_type: Option<String>,
    /// Duration in seconds
    pub duration_secs: Option<f64>,
    /// Container format (e.g. "mp4", "mkv")
    pub container_format: Option<String>,
    /// Indexing status of this file
    pub status: FileIndexStatus,
    /// What kind of content this file represents
    pub content_type: FileContentType,
    /// When this file was first scanned
    pub scanned_at: DateTime<Utc>,
    /// When this file was last updated
    pub updated_at: DateTime<Utc>,
}

/// `path` relative to `root`, with components joined by `/`.
///
/// This is what a client sees of a file's location: NFR-108 forbids a raw
/// filesystem path in a client-facing response, and the library root is the
/// part that would disclose the server's layout.
///
/// A path that is not under `root` -- which the indexer never produces, since
/// it walks from the root -- degrades to the file name alone rather than
/// leaking the absolute path, and is logged so the inconsistency is visible.
/// The comparison is component-wise, so `/m/films2/x.mkv` is not under
/// `/m/films`, and a `..` anywhere below the root counts as not under it.
pub fn root_relative_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).ok().and_then(|relative| {
        relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy()),
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => None,
            })
            .collect::<Option<Vec<_>>>()
            .filter(|components| !components.is_empty())
            .map(|components| components.join("/"))
    });

    match relative {
        Some(relative) => relative,
        None => {
            warn!(
                path = %path.display(),
                root = %root.display(),
                "indexed file is not under its library root; exposing its file name only"
            );
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    }
}

impl LibraryFile {
    /// The client-facing view of `file`, whose location is reported relative
    /// to `library_root` (see [`root_relative_path`]).
    pub fn from_domain(file: beam_domain::models::MediaFile, library_root: &Path) -> Self {
        let beam_domain::models::MediaFile {
            id,
            library_id,
            path,
            hash,
            size_bytes,
            mtime: _,
            mime_type,
            duration,
            container_format,
            status,
            content,
            scanned_at,
            updated_at,
            // Not exposed: every read that feeds this DTO is a visible read of
            // `FileRepository`, which never returns a missing file (#179).
            missing_since: _,
            // Indexer bookkeeping: which rules classified the row.
            classifier_version: _,
            container_tags: _,
        } = file;
        let content_type = match &content {
            Some(beam_domain::models::MediaFileContent::Movie { .. }) => FileContentType::Movie,
            Some(beam_domain::models::MediaFileContent::Episode { .. }) => FileContentType::Episode,
            None => FileContentType::Unclassified,
        };

        LibraryFile {
            id: id.to_string(),
            library_id: library_id.to_string(),
            path: root_relative_path(library_root, &path),
            size_bytes: size_bytes as i64,
            hash: hash.to_string(),
            mime_type,
            duration_secs: duration.map(|d| d.as_secs_f64()),
            container_format,
            status: status.into(),
            content_type,
            scanned_at,
            updated_at,
        }
    }
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
