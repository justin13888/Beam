use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::Serialize;

/// File indexing status
#[derive(Clone, Copy, Debug, Serialize, ToSchema, Eq, PartialEq)]
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
#[derive(Clone, Copy, Debug, Serialize, ToSchema, Eq, PartialEq)]
pub enum FileContentType {
    /// File is associated with a movie
    Movie,
    /// File is associated with a TV episode
    Episode,
    /// Content type is not yet determined
    Unclassified,
}

/// A media file within a library
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LibraryFile {
    pub id: String,
    pub library_id: String,
    /// Filesystem path of the file
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

impl From<beam_domain::models::MediaFile> for LibraryFile {
    fn from(f: beam_domain::models::MediaFile) -> Self {
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
        } = f;
        let content_type = match &content {
            Some(beam_domain::models::MediaFileContent::Movie { .. }) => FileContentType::Movie,
            Some(beam_domain::models::MediaFileContent::Episode { .. }) => FileContentType::Episode,
            None => FileContentType::Unclassified,
        };

        LibraryFile {
            id: id.to_string(),
            library_id: library_id.to_string(),
            path: path.to_string_lossy().to_string(),
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
