use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use sea_orm::DbErr;
use serde_json;
use thiserror::Error;
use tracing::{error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::models::domain::admin_log::{AdminLogCategory, AdminLogLevel};
use crate::models::domain::file::{FileStatus, MediaFileContent, UpdateMediaFile};
use crate::repositories::{
    FileRepository, LibraryRepository, MediaStreamRepository, MovieRepository, ShowRepository,
};
use crate::services::admin_log::AdminLogService;
use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use crate::utils::metadata::{StreamMetadata, VideoFileMetadata};

static EPISODE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)S(\d+)E(\d+)").expect("valid regex"));

// TODO: See if these can be improved. Ensure logic can detect all of them properly
const KNOWN_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "m2ts", "flv", "wmv", "3gp", "ogv", "mpg",
    "mpeg",
];

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("Database error: {0}")]
    Db(#[from] DbErr),
    #[error("Library not found")]
    LibraryNotFound,
    #[error("Invalid Library ID")]
    InvalidId,
    #[error("Path not found: {0}")]
    PathNotFound(String),
}

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait IndexService: Send + Sync + std::fmt::Debug {
    /// Scan a library for new/changed/removed files.
    /// Returns the count of newly added files.
    async fn scan_library(&self, library_id: String) -> Result<u32, IndexError>;
}

#[derive(Debug)]
pub struct LocalIndexService {
    library_repo: Arc<dyn LibraryRepository>,
    file_repo: Arc<dyn FileRepository>,
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
    stream_repo: Arc<dyn MediaStreamRepository>,
    hash_service: Arc<dyn HashService>,
    media_info_service: Arc<dyn MediaInfoService>,
    notification_service: Arc<dyn NotificationService>,
    admin_log: Arc<dyn AdminLogService>,
}

impl LocalIndexService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        library_repo: Arc<dyn LibraryRepository>,
        file_repo: Arc<dyn FileRepository>,
        movie_repo: Arc<dyn MovieRepository>,
        show_repo: Arc<dyn ShowRepository>,
        stream_repo: Arc<dyn MediaStreamRepository>,
        hash_service: Arc<dyn HashService>,
        media_info_service: Arc<dyn MediaInfoService>,
        notification_service: Arc<dyn NotificationService>,
        admin_log: Arc<dyn AdminLogService>,
    ) -> Self {
        Self {
            library_repo,
            file_repo,
            movie_repo,
            show_repo,
            stream_repo,
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
    ) -> Result<u32, IndexError> {
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
        Ok(count)
    }

    /// Classify media content (Movie vs Episode) based on regex
    async fn classify_media_content(
        &self,
        path: &Path,
        lib_uuid: Uuid,
        duration: Duration,
    ) -> Result<MediaFileContent, IndexError> {
        use crate::models::domain::{
            CreateEpisode, CreateMovie, CreateMovieEntry, MediaFileContent,
        };

        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();

        if let Some(captures) = EPISODE_REGEX.captures(&file_stem) {
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
    async fn process_new_file(&self, path: &Path, lib_uuid: Uuid) -> Result<bool, IndexError> {
        use crate::models::domain::CreateMediaFile;

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
            let metadata = std::fs::metadata(path)
                .map_err(|e| IndexError::PathNotFound(format!("Failed to read metadata: {}", e)))?;

            let create_file = CreateMediaFile {
                library_id: lib_uuid,
                path: path.to_path_buf(),
                hash: 0,
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
                let fs_meta = std::fs::metadata(path)
                    .map_err(|ioe| IndexError::PathNotFound(format!("IO Error: {}", ioe)))?;
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
                IndexError::PathNotFound(format!("Hash failed: {}", e))
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
impl IndexService for LocalIndexService {
    async fn scan_library(&self, library_id: String) -> Result<u32, IndexError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| IndexError::InvalidId)?;
        let start_time = chrono::Utc::now();

        // Fetch Library
        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(IndexError::LibraryNotFound)?;

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
            return Err(IndexError::PathNotFound(
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

            if let Some(existing_file) = existing_map.remove(&path) {
                // File exists in DB. Check if changed (size).
                let metadata = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                if metadata.len() != existing_file.size_bytes {
                    info!("File changed: {}", path.display());
                    if existing_file.status != FileStatus::Changed {
                        self.file_repo
                            .update(UpdateMediaFile {
                                id: existing_file.id,
                                hash: None,
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
                            format!("Failed to process file '{}': {}", path.display(), e),
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

        // Phase 4: Remove files that are in DB but not on FS
        let removed_count = existing_map.len();
        let to_remove: Vec<Uuid> = existing_map.values().map(|f| f.id).collect();
        if !to_remove.is_empty() {
            info!("Removing {} missing files from library", to_remove.len());
            self.file_repo.delete_by_ids(to_remove).await?;
        }

        // Update scan finish time
        let end_time = chrono::Utc::now();
        let total_files = self.library_repo.count_files(lib_uuid).await?;

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
                library.name, added_count, removed_count, total_files
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::file::MockFileRepository;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::library::MockLibraryRepository;
    use crate::repositories::library::in_memory::InMemoryLibraryRepository;
    use crate::repositories::movie::MockMovieRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::MockShowRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::stream::MockMediaStreamRepository;
    use crate::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use crate::services::admin_log::NoOpAdminLogService;
    use crate::services::hash::MockHashService;
    use crate::services::media_info::MockMediaInfoService;
    use crate::services::notification::InMemoryNotificationService;
    use crate::utils::metadata::VideoFileMetadata;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ─── helpers ─────────────────────────────────────────────────────────────

    fn make_classify_service() -> (
        LocalIndexService,
        Arc<InMemoryMovieRepository>,
        Arc<InMemoryShowRepository>,
    ) {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let service = LocalIndexService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            movie_repo.clone(),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );
        (service, movie_repo, show_repo)
    }

    // ─── classify_media_content: episode tests ────────────────────────────────

    #[tokio::test]
    async fn test_classify_episode_standard_s01e02() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/Breaking Bad/The.Show.S01E02.mkv");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(3600))
            .await
            .unwrap();

        let episode_id = match content {
            MediaFileContent::Episode { episode_id } => episode_id,
            _ => panic!("expected Episode, got Movie"),
        };

        let episodes: Vec<_> = show_repo
            .episodes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, episode_id);
        assert_eq!(episodes[0].episode_number, 2);

        let seasons: Vec<_> = show_repo
            .seasons
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].season_number, 1);

        let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].title, "Breaking Bad");
    }

    #[tokio::test]
    async fn test_classify_episode_lowercase_pattern() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/My Show/show.s02e10.mp4");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(1800))
            .await
            .unwrap();

        assert!(matches!(content, MediaFileContent::Episode { .. }));

        let episodes: Vec<_> = show_repo
            .episodes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].episode_number, 10);

        let seasons: Vec<_> = show_repo
            .seasons
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(seasons[0].season_number, 2);
    }

    #[tokio::test]
    async fn test_classify_episode_with_resolution_tag() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/shows/Series/Series S01E01 720p.mkv");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(2700))
            .await
            .unwrap();

        assert!(matches!(content, MediaFileContent::Episode { .. }));

        let episodes: Vec<_> = show_repo
            .episodes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(episodes[0].episode_number, 1);

        let seasons: Vec<_> = show_repo
            .seasons
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(seasons[0].season_number, 1);
    }

    #[tokio::test]
    async fn test_classify_episode_show_title_from_parent_dir() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/Breaking Bad/episode.S03E05.mkv");

        service
            .classify_media_content(&path, lib_id, Duration::from_secs(3000))
            .await
            .unwrap();

        let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].title, "Breaking Bad");
    }

    #[tokio::test]
    async fn test_classify_episode_existing_show_reused() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let duration = Duration::from_secs(3600);

        // First call — creates the show
        service
            .classify_media_content(
                &PathBuf::from("/media/My Show/My.Show.S01E01.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        // Second call with same parent dir name — must reuse the existing show
        service
            .classify_media_content(
                &PathBuf::from("/media/My Show/My.Show.S01E02.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
        assert_eq!(shows.len(), 1, "show must not be duplicated");
    }

    #[tokio::test]
    async fn test_classify_episode_new_season_created() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let duration = Duration::from_secs(3600);

        service
            .classify_media_content(
                &PathBuf::from("/media/Show/ep.S01E01.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        service
            .classify_media_content(
                &PathBuf::from("/media/Show/ep.S02E01.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        let mut season_nums: Vec<u32> = show_repo
            .seasons
            .lock()
            .unwrap()
            .values()
            .map(|s| s.season_number)
            .collect();
        season_nums.sort_unstable();
        assert_eq!(season_nums, vec![1, 2]);
    }

    // ─── classify_media_content: movie tests ──────────────────────────────────

    #[tokio::test]
    async fn test_classify_movie_simple_title() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/movies/Avatar.mp4");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(9600))
            .await
            .unwrap();

        let entry_id = match content {
            MediaFileContent::Movie { movie_entry_id } => movie_entry_id,
            _ => panic!("expected Movie, got Episode"),
        };

        let entries: Vec<_> = movie_repo
            .entries
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, entry_id);
        assert!(entries[0].is_primary);

        let movies: Vec<_> = movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].title, "Avatar");
    }

    #[tokio::test]
    async fn test_classify_movie_with_year() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/The.Matrix.Reloaded.2003.mkv");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(7200))
            .await
            .unwrap();

        assert!(matches!(content, MediaFileContent::Movie { .. }));

        let movies: Vec<_> = movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].title, "The.Matrix.Reloaded.2003");
    }

    #[tokio::test]
    async fn test_classify_movie_with_parentheses() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/movie (2024).avi");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(6000))
            .await
            .unwrap();

        assert!(matches!(content, MediaFileContent::Movie { .. }));

        let movies: Vec<_> = movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].title, "movie (2024)");
    }

    #[tokio::test]
    async fn test_classify_movie_existing_reused() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let duration = Duration::from_secs(7200);

        // First call — creates the movie
        service
            .classify_media_content(&PathBuf::from("/media/Avatar.mp4"), lib_id, duration)
            .await
            .unwrap();

        // Second call with the same title — must reuse the existing movie record
        service
            .classify_media_content(&PathBuf::from("/backup/Avatar.mp4"), lib_id, duration)
            .await
            .unwrap();

        let movies: Vec<_> = movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(movies.len(), 1, "movie must not be duplicated");

        // Two distinct entries should exist (one per file path)
        let entries: Vec<_> = movie_repo
            .entries
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert!(entry.is_primary);
        }
    }

    // ─── classify_media_content: edge cases ───────────────────────────────────

    #[tokio::test]
    async fn test_classify_empty_file_stem_falls_to_movie() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        // Root path has no file-stem component — file_stem() returns None → empty string
        let path = PathBuf::from("/");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(100))
            .await
            .unwrap();

        assert!(
            matches!(content, MediaFileContent::Movie { .. }),
            "path with no file stem should fall back to Movie"
        );

        let movies: Vec<_> = movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].title, "");
    }

    #[tokio::test]
    async fn test_classify_episode_no_parent_dir_uses_unknown_show() {
        let (service, _, show_repo) = make_classify_service();
        let lib_id = Uuid::new_v4();
        // Bare filename with no directory component; parent() → Some("") → file_name() → None
        let path = PathBuf::from("S01E01.mkv");

        let content = service
            .classify_media_content(&path, lib_id, Duration::from_secs(3600))
            .await
            .unwrap();

        assert!(matches!(content, MediaFileContent::Episode { .. }));

        let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].title, "Unknown Show");
    }

    #[tokio::test]
    async fn test_process_file_movie_success() {
        let mock_library_repo = MockLibraryRepository::new();
        let mut mock_file_repo = MockFileRepository::new();
        let mut mock_movie_repo = MockMovieRepository::new();
        let mock_show_repo = MockShowRepository::new();
        let mut mock_stream_repo = MockMediaStreamRepository::new();
        let mut mock_hash_service = MockHashService::new();
        let mut mock_media_info_service = MockMediaInfoService::new();

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("movies/Avatar.mp4");
        let lib_id = Uuid::new_v4();

        mock_media_info_service
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| {
                Ok(VideoFileMetadata {
                    file_path: PathBuf::from("test"),
                    metadata: Default::default(),
                    best_video_stream: Some(0),
                    best_audio_stream: Some(1),
                    best_subtitle_stream: None,
                    duration: 1000000,
                    streams: vec![],
                    format_name: "mp4".to_string(),
                    format_long_name: "MPEG-4".to_string(),
                    file_size: 1024,
                    bit_rate: 1000,
                    probe_score: 100,
                })
            });

        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(12345));

        let movie_id = Uuid::new_v4();
        mock_movie_repo
            .expect_find_by_title()
            .times(1)
            .returning(|_| Ok(None));
        mock_movie_repo
            .expect_create()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::Movie {
                    id: movie_id,
                    title: "Avatar".to_string(),
                    title_localized: None,
                    description: None,
                    year: None,
                    release_date: None,
                    runtime: None,
                    poster_url: None,
                    backdrop_url: None,
                    tmdb_id: None,
                    imdb_id: None,
                    tvdb_id: None,
                    rating_tmdb: None,
                    rating_imdb: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                })
            });
        mock_movie_repo
            .expect_ensure_library_association()
            .times(1)
            .returning(|_, _| Ok(()));

        let entry_id = Uuid::new_v4();
        mock_movie_repo
            .expect_create_entry()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::MovieEntry {
                    id: entry_id,
                    library_id: Uuid::new_v4(),
                    movie_id: Uuid::new_v4(),
                    edition: None,
                    is_primary: true,
                    created_at: chrono::Utc::now(),
                })
            });

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(crate::models::domain::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 12345,
                size_bytes: 1024,
                mime_type: Some("video/mp4".to_string()),
                duration: None,
                container_format: None,
                content: Some(crate::models::domain::MediaFileContent::Movie {
                    movie_entry_id: entry_id,
                }),
                status: FileStatus::Known,
                scanned_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });

        mock_stream_repo
            .expect_insert_streams()
            .times(1)
            .returning(|_| Ok(0u32));

        let service = LocalIndexService::new(
            Arc::new(mock_library_repo),
            Arc::new(mock_file_repo),
            Arc::new(mock_movie_repo),
            Arc::new(mock_show_repo),
            Arc::new(mock_stream_repo),
            Arc::new(mock_hash_service),
            Arc::new(mock_media_info_service),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.process_new_file(&path, lib_id).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_process_file_episode_success() {
        let mock_library_repo = MockLibraryRepository::new();
        let mut mock_file_repo = MockFileRepository::new();
        let mock_movie_repo = MockMovieRepository::new();
        let mut mock_show_repo = MockShowRepository::new();
        let mut mock_stream_repo = MockMediaStreamRepository::new();
        let mut mock_hash_service = MockHashService::new();
        let mut mock_media_info_service = MockMediaInfoService::new();

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir
            .path()
            .join("shows/The Show/Season 1/The Show - S01E01.mkv");
        let lib_id = Uuid::new_v4();

        mock_media_info_service
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| {
                Ok(VideoFileMetadata {
                    file_path: PathBuf::from("test"),
                    metadata: Default::default(),
                    best_video_stream: Some(0),
                    best_audio_stream: Some(1),
                    best_subtitle_stream: None,
                    duration: 1800000000,
                    streams: vec![],
                    format_name: "mkv".to_string(),
                    format_long_name: "Matroska".to_string(),
                    file_size: 500 * 1024 * 1024,
                    bit_rate: 2000,
                    probe_score: 100,
                })
            });

        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(67890));

        let show_id = Uuid::new_v4();
        mock_show_repo
            .expect_find_by_title()
            .times(1)
            .returning(|_| Ok(None));
        mock_show_repo.expect_create().times(1).returning(move |_| {
            Ok(crate::models::domain::Show {
                id: show_id,
                title: "Season 1".to_string(),
                title_localized: None,
                description: None,
                year: None,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });
        mock_show_repo
            .expect_ensure_library_association()
            .times(1)
            .returning(|_, _| Ok(()));

        let season_id = Uuid::new_v4();
        mock_show_repo
            .expect_find_or_create_season()
            .times(1)
            .returning(move |_, _| {
                Ok(crate::models::domain::Season {
                    id: season_id,
                    show_id,
                    season_number: 1,
                    poster_url: None,
                    first_aired: None,
                    last_aired: None,
                })
            });

        let episode_id = Uuid::new_v4();
        mock_show_repo
            .expect_create_episode()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::Episode {
                    id: episode_id,
                    season_id,
                    episode_number: 1,
                    title: "The Show - S01E01".to_string(),
                    description: None,
                    air_date: None,
                    runtime: None,
                    thumbnail_url: None,
                    created_at: chrono::Utc::now(),
                })
            });

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(crate::models::domain::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 67890,
                size_bytes: 500 * 1024 * 1024,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: None,
                content: Some(crate::models::domain::MediaFileContent::Episode { episode_id }),
                status: FileStatus::Known,
                scanned_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });

        mock_stream_repo
            .expect_insert_streams()
            .times(1)
            .returning(|_| Ok(0u32));

        let service = LocalIndexService::new(
            Arc::new(mock_library_repo),
            Arc::new(mock_file_repo),
            Arc::new(mock_movie_repo),
            Arc::new(mock_show_repo),
            Arc::new(mock_stream_repo),
            Arc::new(mock_hash_service),
            Arc::new(mock_media_info_service),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.process_new_file(&path, lib_id).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }
}
