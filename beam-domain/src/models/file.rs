use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

/// Represents a media file in the library
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub id: Uuid,
    pub library_id: Uuid,
    pub path: PathBuf,
    pub hash: u64,
    pub size_bytes: u64,
    /// Filesystem modification time; paired with `size_bytes` for change detection.
    pub mtime: Option<DateTime<Utc>>,
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
    pub scanned_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Status of the file in the library
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum FileStatus {
    /// File is indexed and metadata matches
    Known,
    /// File exists but metadata/hash has changed
    Changed,
    /// File exists but extension is unknown/unsupported
    Unknown,
}

// `Display`/`FromStr` used to exist here to move this enum in and out of the
// `files.file_status` column as text. That was the bug: the column is a
// Postgres enum type, and binding text to it fails at runtime. The conversion
// now goes through `beam_entity::files::FileStatus` (a `DeriveActiveEnum`),
// which leaves the string forms with no callers -- so they are gone rather
// than kept as an untested second way to spell the same values.

/// The content type of a media file
#[derive(Debug, Clone)]
pub enum MediaFileContent {
    /// File is a movie
    Movie { movie_entry_id: Uuid },
    /// File is a TV episode
    Episode { episode_id: Uuid },
}

/// Parameters for creating a new media file
#[derive(Debug, Clone)]
pub struct CreateMediaFile {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub hash: u64,
    pub size_bytes: u64,
    pub mtime: Option<DateTime<Utc>>,
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
}

/// Parameters for updating an existing media file
#[derive(Debug, Clone)]
pub struct UpdateMediaFile {
    pub id: Uuid,
    pub hash: Option<u64>,
    pub size_bytes: Option<u64>,
    /// `Some` sets the stored mtime; `None` leaves it unchanged.
    pub mtime: Option<DateTime<Utc>>,
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: Option<FileStatus>,
}

#[cfg(feature = "entity")]
impl From<beam_entity::files::FileStatus> for FileStatus {
    fn from(status: beam_entity::files::FileStatus) -> Self {
        match status {
            beam_entity::files::FileStatus::Known => FileStatus::Known,
            beam_entity::files::FileStatus::Changed => FileStatus::Changed,
            beam_entity::files::FileStatus::Unknown => FileStatus::Unknown,
        }
    }
}

#[cfg(feature = "entity")]
impl From<FileStatus> for beam_entity::files::FileStatus {
    fn from(status: FileStatus) -> Self {
        match status {
            FileStatus::Known => beam_entity::files::FileStatus::Known,
            FileStatus::Changed => beam_entity::files::FileStatus::Changed,
            FileStatus::Unknown => beam_entity::files::FileStatus::Unknown,
        }
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::files::Model> for MediaFile {
    fn from(model: beam_entity::files::Model) -> Self {
        let content = model
            .movie_entry_id
            .map(|id| MediaFileContent::Movie { movie_entry_id: id })
            .or_else(|| {
                model
                    .episode_id
                    .map(|id| MediaFileContent::Episode { episode_id: id })
            });

        Self {
            id: model.id,
            library_id: model.library_id,
            path: PathBuf::from(model.file_path),
            hash: model.hash_xxh3 as u64,
            size_bytes: model.file_size as u64,
            mtime: model.mtime.map(|d| d.with_timezone(&Utc)),
            mime_type: model.mime_type,
            duration: model.duration_secs.map(Duration::from_secs_f64),
            container_format: model.container_format,
            content,
            status: FileStatus::from(model.file_status),
            scanned_at: model.scanned_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}
