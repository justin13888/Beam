use chrono::{DateTime, Utc};
use std::fmt;
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

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileStatus::Known => write!(f, "known"),
            FileStatus::Changed => write!(f, "changed"),
            FileStatus::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for FileStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "known" => Ok(FileStatus::Known),
            "changed" => Ok(FileStatus::Changed),
            "unknown" => Ok(FileStatus::Unknown),
            _ => Err(format!("Invalid file status: {}", s)),
        }
    }
}

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
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: Option<FileStatus>,
}

impl From<beam_entity::files::Model> for MediaFile {
    fn from(model: beam_entity::files::Model) -> Self {
        // Determine content type from polymorphic foreign keys
        let content = model
            .movie_entry_id
            .map(|id| MediaFileContent::Movie { movie_entry_id: id })
            .or_else(|| {
                model
                    .episode_id
                    .map(|id| MediaFileContent::Episode { episode_id: id })
            });

        let status = model.file_status.parse().unwrap_or(FileStatus::Unknown);

        Self {
            id: model.id,
            library_id: model.library_id,
            path: PathBuf::from(model.file_path),
            hash: model.hash_xxh3 as u64,
            size_bytes: model.file_size as u64,
            mime_type: model.mime_type,
            duration: model.duration_secs.map(Duration::from_secs_f64),
            container_format: model.container_format,
            content,
            status,
            scanned_at: model.scanned_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}
