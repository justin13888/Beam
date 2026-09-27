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
    /// When the indexer first found this file gone from disk; `None` while it
    /// is present. A missing file is soft-deleted: every visible read of
    /// [`crate::repositories::FileRepository`] skips it, and it keeps its id
    /// -- and so its playback progress -- until the path reappears or the
    /// grace period runs out (issue #179).
    pub missing_since: Option<DateTime<Utc>>,
    /// The version of the path-classification rules that decided `content`
    /// (see [`crate::utils::media_path::CLASSIFIER_VERSION`]). A row decided
    /// by older rules is reclassified from its path by the next scan.
    pub classifier_version: u16,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaFileContent {
    /// File is a movie
    Movie { movie_entry_id: Uuid },
    /// File is a TV episode -- or, when `last_episode_number` is set, the
    /// run of episodes from `episode_id`'s up to that number that one
    /// multi-episode file holds (`S01E01E02`). The file is attached to the
    /// first episode; the rest of the range is recorded here rather than as
    /// extra rows.
    Episode {
        episode_id: Uuid,
        last_episode_number: Option<u32>,
    },
}

impl MediaFileContent {
    /// A single-episode file of `episode_id`.
    pub fn episode(episode_id: Uuid) -> Self {
        MediaFileContent::Episode {
            episode_id,
            last_episode_number: None,
        }
    }
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
    /// See [`MediaFile::classifier_version`].
    pub classifier_version: u16,
}

/// A file's classification, replaced as a whole when a scan reclassifies it
/// under newer rules. `content: None` clears it: the path no longer says what
/// the file is.
#[derive(Debug, Clone)]
pub struct FileClassification {
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
    pub classifier_version: u16,
}

/// Parameters for updating an existing media file
#[derive(Debug, Clone)]
pub struct UpdateMediaFile {
    pub id: Uuid,
    pub hash: Option<u64>,
    pub size_bytes: Option<u64>,
    /// `Some` sets the stored mtime; `None` leaves it unchanged.
    pub mtime: Option<DateTime<Utc>>,
    pub probe: ProbeUpdate,
    pub content: Option<MediaFileContent>,
    pub status: Option<FileStatus>,
}

/// What an [`UpdateMediaFile`] does to a file's probe results -- its MIME
/// type, duration and container format, which one probe sets together.
///
/// A row with no duration is one whose probe has not succeeded, and the
/// indexer probes it again on every visit (FR-218). So a failed probe of
/// changed content must [`ProbeUpdate::Clear`] them: kept, the old content's
/// results would describe a file that is gone and the row would never be
/// probed again.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeUpdate {
    /// Leave them as they are.
    Keep,
    /// Replace them with a successful probe's.
    Set {
        mime_type: String,
        duration: Duration,
        container_format: String,
    },
    /// Clear all three: the content changed and its probe failed.
    Clear,
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
                model.episode_id.map(|id| MediaFileContent::Episode {
                    episode_id: id,
                    last_episode_number: model.last_episode_number.map(|n| n as u32),
                })
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
            missing_since: model.missing_since.map(|d| d.with_timezone(&Utc)),
            classifier_version: model.classifier_version as u16,
        }
    }
}
