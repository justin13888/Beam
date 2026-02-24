use std::sync::Arc;

use regex::Regex;
use sea_orm::DbErr;
use serde_json;
use thiserror::Error;
use tracing::{error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::models::domain::Library as DomainLibrary;
use crate::models::domain::admin_log::{AdminLogCategory, AdminLogLevel};
use crate::models::domain::file::{FileStatus, MediaFileContent, UpdateMediaFile};
use crate::models::{Library, LibraryFile};
use crate::services::admin_log::AdminLogService;
use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use crate::utils::metadata::{StreamMetadata, VideoFileMetadata};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// TODO: See if these can be improved. Ensure logic can detect all of them properly
const KNOWN_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "m2ts", "flv", "wmv", "3gp", "ogv", "mpg",
    "mpeg",
];

#[async_trait::async_trait]
pub trait LibraryService: Send + Sync + std::fmt::Debug {
    /// Get all libraries by user ID
    /// Returns None if user is not found
    async fn get_libraries(&self, user_id: String) -> Result<Vec<Library>, LibraryError>;

    /// Get a single library by ID
    async fn get_library_by_id(&self, library_id: String) -> Result<Option<Library>, LibraryError>;

    /// Get all files within a library
    async fn get_library_files(&self, library_id: String)
    -> Result<Vec<LibraryFile>, LibraryError>;

    /// Create a new library
    async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<Library, LibraryError>;

    /// Scan a library for new content
    async fn scan_library(&self, library_id: String) -> Result<u32, LibraryError>;

    /// Delete a library by ID
    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError>;
}

#[derive(Debug)]
pub struct LocalLibraryService {
    library_repo: Arc<dyn crate::repositories::LibraryRepository>,
    file_repo: Arc<dyn crate::repositories::FileRepository>,
    movie_repo: Arc<dyn crate::repositories::MovieRepository>,
    show_repo: Arc<dyn crate::repositories::ShowRepository>,
    stream_repo: Arc<dyn crate::repositories::MediaStreamRepository>,
    video_dir: PathBuf,
    hash_service: Arc<dyn HashService>,
    media_info_service: Arc<dyn MediaInfoService>,
    notification_service: Arc<dyn NotificationService>,
    admin_log: Arc<dyn AdminLogService>,
}

impl LocalLibraryService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        library_repo: Arc<dyn crate::repositories::LibraryRepository>,
        file_repo: Arc<dyn crate::repositories::FileRepository>,
        movie_repo: Arc<dyn crate::repositories::MovieRepository>,
        show_repo: Arc<dyn crate::repositories::ShowRepository>,
        stream_repo: Arc<dyn crate::repositories::MediaStreamRepository>,
        video_dir: PathBuf,
        hash_service: Arc<dyn HashService>,
        media_info_service: Arc<dyn MediaInfoService>,
        notification_service: Arc<dyn NotificationService>,
        admin_log: Arc<dyn AdminLogService>,
    ) -> Self {
        LocalLibraryService {
            library_repo,
            file_repo,
            movie_repo,
            show_repo,
            stream_repo,
            video_dir,
            hash_service,
            media_info_service,
            notification_service,
            admin_log,
        }
    }

    /// Helper to extract and insert media streams for a file
    async fn insert_media_streams(
        &self,
        file_id: Uuid,
        metadata: &VideoFileMetadata,
    ) -> Result<u32, LibraryError> {
        use crate::models::domain::stream::{
            AudioStreamMetadata, SubtitleStreamMetadata, VideoStreamMetadata,
        };
        use crate::models::domain::{
            CreateMediaStream, StreamMetadata as DomainStreamMetadata, StreamType,
        };

        let mut streams_to_insert = Vec::new();

        for stream in &metadata.streams {
            let (stream_metadata, stream_type) = match stream {
                StreamMetadata::Video(v) => {
                    let metadata = DomainStreamMetadata::Video(VideoStreamMetadata {
                        width: v.video.width,
                        height: v.video.height,
                        frame_rate: v.frame_rate(),
                        bit_rate: Some(v.video.bit_rate as u64),
                        color_space: None,
                        color_range: None,
                        hdr_format: None,
                    });
                    (metadata, StreamType::Video)
                }
                StreamMetadata::Audio(a) => {
                    let metadata = DomainStreamMetadata::Audio(AudioStreamMetadata {
                        language: Some(a.audio.language.clone()).filter(|s| !s.is_empty()),
                        title: Some(a.audio.title.clone()).filter(|s| !s.is_empty()),
                        channels: a.audio.channels,
                        sample_rate: a.audio.rate,
                        channel_layout: Some(a.audio.channel_layout_description().to_string()),
                        bit_rate: Some(a.audio.bit_rate as u64),
                        is_default: false,
                        is_forced: false,
                    });
                    (metadata, StreamType::Audio)
                }
                StreamMetadata::Subtitle(s) => {
                    let metadata = DomainStreamMetadata::Subtitle(SubtitleStreamMetadata {
                        language: s.language(),
                        title: s.title(),
                        is_default: false,
                        is_forced: false,
                    });
                    (metadata, StreamType::Subtitle)
                }
            };

            streams_to_insert.push(CreateMediaStream {
                file_id,
                index: stream.index() as u32,
                stream_type,
                codec: match stream {
                    StreamMetadata::Video(v) => v.video.codec_name.clone(),
                    StreamMetadata::Audio(a) => a.audio.codec_name.clone(),
                    StreamMetadata::Subtitle(s) => format!("{:?}", s.codec_id),
                },
                metadata: stream_metadata,
            });
        }

        let count = self.stream_repo.insert_streams(streams_to_insert).await?;
        Ok(count as u32)
    }

    /// Classify media content (Movie vs Episode) based on regex
    async fn classify_media_content(
        &self,
        path: &Path,
        lib_uuid: Uuid,
        duration: Duration,
    ) -> Result<MediaFileContent, LibraryError> {
        use crate::models::domain::{
            CreateEpisode, CreateMovie, CreateMovieEntry, MediaFileContent,
        };

        // Regex for detecting SxxExx pattern
        let episode_regex = Regex::new(r"(?i)S(\d+)E(\d+)").unwrap();

        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();

        if let Some(captures) = episode_regex.captures(&file_stem) {
            // IT IS AN EPISODE
            let season_num: u32 = captures[1].parse().unwrap_or(1);
            let episode_num: i32 = captures[2].parse().unwrap_or(1);

            // Show title guess: Parent directory name
            let show_title = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown Show".to_string());

            // Find or create show using repository
            let show = match self.show_repo.find_by_title(&show_title).await? {
                Some(s) => s,
                None => self.show_repo.create(show_title.clone()).await?,
            };

            // Ensure library-show association exists
            self.show_repo
                .ensure_library_association(lib_uuid, show.id)
                .await?;

            // Find or create season
            let season = self
                .show_repo
                .find_or_create_season(show.id, season_num)
                .await?;

            // Create episode
            let create_episode = CreateEpisode {
                season_id: season.id,
                episode_number: episode_num as u32,
                title: file_stem.to_string(),
                runtime: Some(duration),
            };
            let episode = self.show_repo.create_episode(create_episode).await?;

            Ok(MediaFileContent::Episode {
                episode_id: episode.id,
            })
        } else {
            // IT IS A MOVIE
            let movie_title = file_stem.to_string();

            // Find or create movie using repository
            let movie = match self.movie_repo.find_by_title(&movie_title).await? {
                Some(m) => m,
                None => {
                    let create_movie = CreateMovie {
                        title: movie_title,
                        runtime: Some(duration),
                    };
                    self.movie_repo.create(create_movie).await?
                }
            };

            // Ensure library-movie association exists
            self.movie_repo
                .ensure_library_association(lib_uuid, movie.id)
                .await?;

            // Create movie entry
            let create_entry = CreateMovieEntry {
                library_id: lib_uuid,
                movie_id: movie.id,
                edition: None,
                is_primary: true,
            };
            let entry = self.movie_repo.create_entry(create_entry).await?;

            Ok(MediaFileContent::Movie {
                movie_entry_id: entry.id,
            })
        }
    }

    /// Process a NEW file to add it to the library
    async fn process_new_file(&self, path: &Path, lib_uuid: Uuid) -> Result<bool, LibraryError> {
        use crate::models::domain::CreateMediaFile;
        use std::time::Duration;

        info!("Processing new file: {}", path.display());

        // Check extension
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let is_known_video = KNOWN_VIDEO_EXTENSIONS.contains(&ext.as_str());

        if !is_known_video {
            // Index as Unknown file
            let metadata = std::fs::metadata(path).map_err(|e| {
                LibraryError::PathNotFound(format!("Failed to read metadata: {}", e))
            })?;

            let create_file = CreateMediaFile {
                library_id: lib_uuid,
                path: path.to_path_buf(),
                hash: 0, // No hash for unknown files? Or partial hash? checking task.md: "store in files table, mark status as unknown"
                size_bytes: metadata.len(),
                mime_type: None,
                duration: None,
                container_format: None,
                content: None,
                status: FileStatus::Unknown,
            };
            self.file_repo.create(create_file).await?;
            return Ok(true);
        }

        // Known video: Extract Metadata and Hash
        let metadata = match self.media_info_service.get_video_metadata(path).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to extract metadata for {}: {}", path.display(), e);
                // Fallback to Unknown status if metadata extraction fails?
                // Or allow it to fail?
                // Plan says "handle gracefully".
                // I'll create as Unknown if metadata fails.
                let fs_meta = std::fs::metadata(path)
                    .map_err(|ioe| LibraryError::PathNotFound(format!("IO Error: {}", ioe)))?;
                let create_file = CreateMediaFile {
                    library_id: lib_uuid,
                    path: path.to_path_buf(),
                    hash: 0,
                    size_bytes: fs_meta.len(),
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: FileStatus::Unknown,
                };
                self.file_repo.create(create_file).await?;
                return Ok(true);
            }
        };

        let hash_value = self
            .hash_service
            .hash_async(path.to_path_buf())
            .await
            .map_err(|e| {
                error!("Failed to hash file {}: {}", path.display(), e);
                LibraryError::PathNotFound(format!("Hash failed: {}", e))
            })?;

        // Classify content
        let duration = Duration::from_secs_f64(metadata.duration_seconds());
        let content = self
            .classify_media_content(path, lib_uuid, duration)
            .await?;

        // Create media file
        let create_file = CreateMediaFile {
            library_id: lib_uuid,
            path: path.to_path_buf(),
            hash: hash_value,
            size_bytes: metadata.file_size,
            mime_type: Some(format!("video/{}", metadata.format_name)),
            duration: Some(duration),
            container_format: Some(metadata.format_name.clone()),
            content: Some(content),
            status: FileStatus::Known,
        };

        let file = self.file_repo.create(create_file).await?;

        // Extract and insert media streams
        self.insert_media_streams(file.id, &metadata).await?;

        Ok(true)
    }
}

#[async_trait::async_trait]
impl LibraryService for LocalLibraryService {
    /// Get all libraries by user ID
    /// Returns None if user is not found
    async fn get_libraries(&self, _user_id: String) -> Result<Vec<Library>, LibraryError> {
        let domain_libraries = self.library_repo.find_all().await?;

        // Convert domain models to GraphQL models
        let mut result = Vec::new();
        for lib in domain_libraries {
            let DomainLibrary {
                id,
                name,
                root_path: _,
                description,
                created_at: _,
                updated_at: _,
                last_scan_started_at,
                last_scan_finished_at,
                last_scan_file_count,
            } = lib;
            let size = self.library_repo.count_files(lib.id).await?;

            result.push(Library {
                id: id.to_string(),
                name,
                description,
                size: size as u32,
                last_scan_started_at: last_scan_started_at.map(|d| d.with_timezone(&chrono::Utc)),
                last_scan_finished_at: last_scan_finished_at.map(|d| d.with_timezone(&chrono::Utc)),
                last_scan_file_count,
            });
        }

        Ok(result)
    }

    async fn get_library_by_id(&self, library_id: String) -> Result<Option<Library>, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;
        let library = self.library_repo.find_by_id(lib_uuid).await?;

        match library {
            Some(lib) => {
                let size = self.library_repo.count_files(lib.id).await?;
                Ok(Some(Library {
                    id: lib.id.to_string(),
                    name: lib.name,
                    description: lib.description,
                    size: size as u32,
                    last_scan_started_at: lib.last_scan_started_at,
                    last_scan_finished_at: lib.last_scan_finished_at,
                    last_scan_file_count: lib.last_scan_file_count,
                }))
            }
            None => Ok(None),
        }
    }

    async fn get_library_files(
        &self,
        library_id: String,
    ) -> Result<Vec<LibraryFile>, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;

        // Verify library exists
        self.library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        let files = self.file_repo.find_all_by_library(lib_uuid).await?;
        Ok(files.into_iter().map(LibraryFile::from).collect())
    }

    /// Create a new library
    async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<Library, LibraryError> {
        use crate::models::domain::CreateLibrary;
        use std::path::PathBuf;

        let requested_path = PathBuf::from(&root_path);

        // Validate that the requested path is a child of video_dir
        // Canonicalize both paths to resolve symlinks and absolute paths
        let canonical_video_dir = self.video_dir.canonicalize().map_err(|e| {
            error!("Failed to canonicalize video_dir: {}", e);
            LibraryError::PathNotFound(self.video_dir.to_string_lossy().to_string())
        })?;

        // Note: requested_path might not exist yet?
        // If it doesn't exist, we can't canonicalize it easily to check prefix safely without doing string manipulation which is prone to traversal attacks.
        // Option 1: Require it to exist.
        // Option 2: Check absolute path string prefix (less secure against symlinks).

        // For now, let's require it to exist or at least verify it doesn't contain '..' components if we can't canonicalize.
        // Actually, preventing '..' in string is safer if valid path.
        // Better: Join video_dir with root_path?
        // User passes "root_path" as absolute path or relative?
        // The API says "root_path".
        // If absolute, we check starts_with.

        // If the path is absolute
        let target_path = if requested_path.is_absolute() {
            requested_path
        } else {
            self.video_dir.join(requested_path)
        };

        // We try to canonicalize target_path. If it fails (doesn't exist), we can't verify secure containment easily.
        // Let's assume the user MUST provide a path that exists for now, or we create it?
        // BEAM is read-only for media, so it MUST exist.

        let canonical_target = target_path.canonicalize().map_err(|e| {
            LibraryError::PathNotFound(format!("Library path does not exist or invalid: {}", e))
        })?;

        if !canonical_target.starts_with(&canonical_video_dir) {
            return Err(LibraryError::Validation(format!(
                "Library path must be within the video directory: {}",
                self.video_dir.display()
            )));
        }

        let create = CreateLibrary {
            name: name.clone(),
            root_path: canonical_target,
            description: None,
        };

        let DomainLibrary {
            id,
            name,
            root_path: _,
            description,
            created_at: _,
            updated_at: _,
            last_scan_started_at,
            last_scan_finished_at,
            last_scan_file_count,
        } = self.library_repo.create(create).await?;

        self.notification_service.publish(AdminEvent::info(
            EventCategory::System,
            format!("Library '{}' created", name),
            Some(id.to_string()),
            Some(name.clone()),
        ));

        Ok(Library {
            id: id.to_string(),
            name,
            description,
            size: 0,
            last_scan_started_at,
            last_scan_finished_at,
            last_scan_file_count,
        })
    }

    /// Scan a library for new content (Reconciliation: Phase 1, 2, 3)
    async fn scan_library(&self, library_id: String) -> Result<u32, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;
        let start_time = chrono::Utc::now();

        // Fetch Library
        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        info!(
            "Scanning library: {} ({:?})",
            library.name, library.root_path
        );

        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            format!("Library scan started for '{}'", library.name),
            Some(lib_uuid.to_string()),
            Some(library.name.clone()),
        ));
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                format!("Library scan started: \"{}\"", library.name),
                Some(serde_json::json!({ "library_id": library_id, "path": library.root_path })),
            )
            .await;

        // Update scan start time
        self.library_repo
            .update_scan_progress(lib_uuid, Some(start_time), None, None)
            .await?;

        if !library.root_path.exists() {
            self.notification_service.publish(AdminEvent::error(
                EventCategory::LibraryScan,
                format!(
                    "Library '{}' path not found: {}",
                    library.name,
                    library.root_path.display()
                ),
                Some(lib_uuid.to_string()),
                Some(library.name.clone()),
            ));
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Error,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "Library scan failed: path not found for \"{}\"",
                        library.name
                    ),
                    Some(serde_json::json!({
                        "library_id": library_id,
                        "path": library.root_path
                    })),
                )
                .await;
            return Err(LibraryError::PathNotFound(
                library.root_path.to_string_lossy().to_string(),
            ));
        }

        // Phase 1: Fetch existing files from DB
        let existing_files = self.file_repo.find_all_by_library(lib_uuid).await?;
        let mut existing_map: HashMap<PathBuf, crate::models::domain::MediaFile> = existing_files
            .into_iter()
            .map(|f| (f.path.clone(), f))
            .collect();

        info!("Found {} existing files in DB", existing_map.len());

        let mut added_count = 0;

        // Phase 2 & 3: Walk FS, compare with DB, add new files
        for entry in WalkDir::new(&library.root_path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }

            // Check if file is known (any video extension or previously indexed unknown)
            // Note: We deliberately don't filter extensions strictly here because we want to
            // handle files that WERE known but maybe changed ext?
            // Actually, we process EVERYTHING in WalkDir?
            // Better: Filter extensions for NEW files. Existing files we check regardless.

            if let Some(existing_file) = existing_map.remove(&path) {
                // File exists in DB. Check if changed (size/mtime).
                // For now, simplicity: checking size.
                let metadata = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue, // Can't read file, skip
                };

                if metadata.len() != existing_file.size_bytes {
                    info!("File changed: {}", path.display());
                    // Mark as Changed
                    if existing_file.status != FileStatus::Changed {
                        self.file_repo
                            .update(UpdateMediaFile {
                                id: existing_file.id,
                                hash: None, // We don't re-hash immediately to save perf? Or should we?
                                size_bytes: Some(metadata.len()),
                                mime_type: None,
                                duration: None,
                                container_format: None,
                                content: None,
                                status: Some(FileStatus::Changed),
                            })
                            .await?;
                    }
                }
            } else {
                // New file
                match self.process_new_file(&path, lib_uuid).await {
                    Ok(true) => added_count += 1,
                    Ok(false) => {}
                    Err(e) => {
                        error!("Failed to process file {}: {}", path.display(), e);
                        self.notification_service.publish(AdminEvent::warning(
                            EventCategory::LibraryScan,
                            format!(
                                "Failed to process file '{}': {}",
                                path.display(),
                                e
                            ),
                            Some(lib_uuid.to_string()),
                            Some(library.name.clone()),
                        ));
                        let _ = self
                            .admin_log
                            .log(
                                AdminLogLevel::Warning,
                                AdminLogCategory::LibraryScan,
                                format!("Failed to process file: {}", path.display()),
                                Some(serde_json::json!({
                                    "library_id": library_id,
                                    "path": path.display().to_string(),
                                    "error": e.to_string()
                                })),
                            )
                            .await;
                    }
                }
            }
        }

        // Phase 4: Remove files that are in DB but not on FS (remaining in map)
        let removed_count = existing_map.len();
        let to_remove: Vec<Uuid> = existing_map.values().map(|f| f.id).collect();
        if !to_remove.is_empty() {
            info!("Removing {} missing files from library", to_remove.len());
            self.file_repo.delete_by_ids(to_remove).await?;
        }

        // Update scan finish time
        let end_time = chrono::Utc::now();
        let total_files = self.library_repo.count_files(lib_uuid).await?; // Recount total

        self.library_repo
            .update_scan_progress(lib_uuid, None, Some(end_time), Some(total_files as i32))
            .await?;

        info!(
            "Scan complete. Added: {}, Removed: {}, Total: {}",
            added_count, removed_count, total_files
        );

        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            format!(
                "Library scan complete for '{}': added {}, removed {}, total {}",
                library.name,
                added_count,
                existing_map.len(),
                total_files
            ),
            Some(lib_uuid.to_string()),
            Some(library.name.clone()),
        ));
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                format!(
                    "Library scan completed: \"{}\" — {} added, {} removed, {} total",
                    library.name, added_count, removed_count, total_files
                ),
                Some(serde_json::json!({
                    "library_id": library_id,
                    "added": added_count,
                    "removed": removed_count,
                    "total": total_files,
                })),
            )
            .await;

        Ok(added_count)
    }

    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;

        // Verify library exists
        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        // Delete the library itself
        self.library_repo.delete(lib_uuid).await?;

        self.notification_service.publish(AdminEvent::info(
            EventCategory::System,
            format!("Library '{}' deleted", library.name),
            Some(lib_uuid.to_string()),
            Some(library.name),
        ));

        Ok(true)
    }
}

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("User not found")]
    UserNotFound,
    #[error("Database error: {0}")]
    Db(#[from] DbErr),

    #[error("Library not found")]
    LibraryNotFound,
    #[error("Invalid Library ID")]
    InvalidId,
    #[error("Path not found: {0}")]
    PathNotFound(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod library_tests;
