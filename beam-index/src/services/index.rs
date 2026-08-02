use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use serde_json;
use thiserror::Error;
use tracing::{error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::probe::metadata::{StreamMetadata, VideoFileMetadata};
use crate::services::admin_log::AdminLogService;
use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use crate::services::watcher::FsEventKind;
use beam_domain::models::admin_log::{AdminLogCategory, AdminLogLevel};
use beam_domain::models::file::{
    CreateMediaFile, FileStatus, MediaFile, MediaFileContent, UpdateMediaFile,
};
use beam_domain::repositories::{
    EnrichmentStateRepository, FileRepository, LibraryRepository, MediaStreamRepository,
    MovieRepository, ShowRepository,
};

// TODO: See if these can be improved. Ensure logic can detect all of them properly
const KNOWN_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "m2ts", "flv", "wmv", "3gp", "ogv", "mpg",
    "mpeg",
];

/// Read the size and modification time of a file in a single stat call.
fn read_fs_meta(path: &Path) -> std::io::Result<(u64, Option<DateTime<Utc>>)> {
    let meta = std::fs::metadata(path)?;
    let mtime: Option<DateTime<Utc>> = meta.modified().ok().map(|t| t.into());
    Ok((meta.len(), mtime))
}

/// Whether a path has a recognised video file extension.
fn is_known_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .is_some_and(|e| KNOWN_VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// Records one processed-file outcome on the
/// `beam_index_files_processed_total{result}` counter, covering both full
/// scans and watcher-driven reconciles. `result` is one of `new`, `changed`,
/// `unchanged`, or `failed`. A no-op unless beam-server installed a metrics
/// recorder (`BEAM_ENABLE_METRICS=true`).
fn record_file_outcome(result: &'static str) {
    metrics::counter!("beam_index_files_processed_total", "result" => result).increment(1);
}

/// Render a runtime in whole minutes for human-facing warnings (e.g. `40 min`).
fn humanize_minutes(secs: f64) -> String {
    format!("{} min", (secs / 60.0).round() as i64)
}

/// Thresholds for flagging renditions of the same title whose probed runtimes
/// disagree by enough to suggest a misnamed or mismatched file (issue #88).
///
/// A pair of renditions is treated as *divergent* only when BOTH conditions
/// hold: the relative difference `|a - b| / max(a, b)` exceeds
/// `max_runtime_ratio` AND the absolute difference exceeds
/// `min_runtime_delta_secs`. The double condition keeps the check quiet in the
/// two cases where a single threshold is noisy -- on short content, where a
/// large ratio can still be only a few seconds, and on long content, where a
/// few minutes of legitimate edition drift is a tiny ratio.
#[derive(Debug, Clone, Copy)]
pub struct DivergencePolicy {
    /// Minimum relative runtime difference (`0.0`–`1.0`) for a pair to count as
    /// diverging. Defaults to `0.15` (15%).
    pub max_runtime_ratio: f64,
    /// Minimum absolute runtime difference, in seconds, for a pair to count as
    /// diverging. Defaults to `240.0` (4 minutes).
    pub min_runtime_delta_secs: f64,
}

impl Default for DivergencePolicy {
    fn default() -> Self {
        Self {
            max_runtime_ratio: 0.15,
            min_runtime_delta_secs: 240.0,
        }
    }
}

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
    hash_unknown_files: bool,
    enrichment_repo: Option<Arc<dyn EnrichmentStateRepository>>,
    divergence_policy: DivergencePolicy,
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
            hash_unknown_files: true,
            enrichment_repo: None,
            divergence_policy: DivergencePolicy::default(),
        }
    }

    /// Override the runtime-divergence thresholds used when warning that two
    /// renditions of the same movie/episode disagree on runtime. Defaults to
    /// [`DivergencePolicy::default`].
    pub fn with_divergence_policy(mut self, policy: DivergencePolicy) -> Self {
        self.divergence_policy = policy;
        self
    }

    /// Override whether files with unknown extensions are hashed for duplicate
    /// detection. Defaults to `true`.
    pub fn with_hash_unknown_files(mut self, value: bool) -> Self {
        self.hash_unknown_files = value;
        self
    }

    /// Wire up the enrichment queue: when set, every movie/show
    /// found-or-created during classification gets a `Pending` enrichment row
    /// (idempotent -- a no-op if one already exists). Defaults to `None`,
    /// which disables enrichment-queue bookkeeping entirely.
    pub fn with_enrichment_repo(mut self, repo: Arc<dyn EnrichmentStateRepository>) -> Self {
        self.enrichment_repo = Some(repo);
        self
    }

    /// The repository backing this service's library lookups, exposed so
    /// callers (e.g. background maintenance tasks) can list/query libraries
    /// without needing their own separate handle to the same repository.
    pub fn library_repo(&self) -> &Arc<dyn LibraryRepository> {
        &self.library_repo
    }

    /// Helper to extract and insert media streams for a file
    pub(crate) async fn insert_media_streams(
        &self,
        file_id: Uuid,
        metadata: &VideoFileMetadata,
    ) -> Result<u32, IndexError> {
        use beam_domain::models::stream::{
            AudioStreamMetadata, SubtitleStreamMetadata, VideoStreamMetadata,
        };
        use beam_domain::models::{
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
                        bit_rate: Some(v.video.bit_rate),
                        color_space: Some(v.video.color_space.description().to_string()),
                        color_range: Some(v.video.color_range.description().to_string()),
                        hdr_format: v
                            .video
                            .color_transfer_characteristic
                            .hdr_format_name()
                            .map(|s| s.to_string()),
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
                        bit_rate: Some(a.audio.bit_rate),
                        is_default: a.disposition.is_default(),
                        is_forced: a.disposition.is_forced(),
                    });
                    (metadata, StreamType::Audio)
                }
                StreamMetadata::Subtitle(s) => {
                    let metadata = DomainStreamMetadata::Subtitle(SubtitleStreamMetadata {
                        language: s.language(),
                        title: s.title(),
                        is_default: s.disposition.is_default(),
                        is_forced: s.disposition.is_forced(),
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

    /// Classify media content (Movie vs Episode) using the scene-filename parser.
    async fn classify_media_content(
        &self,
        path: &Path,
        lib_uuid: Uuid,
        duration: Duration,
    ) -> Result<MediaFileContent, IndexError> {
        use beam_domain::models::{
            CreateEpisode, CreateMovie, CreateMovieEntry, CreateShow, MediaFileContent,
        };
        use beam_domain::utils::filename::parse_media_filename;

        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        let parsed = parse_media_filename(&file_stem);

        if let (Some(season_num), Some(episode_num)) = (parsed.season, parsed.episode) {
            // IT IS AN EPISODE

            // Show title/year guess: parent directory name, parsed the same way.
            let dir_name = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parsed_show = parse_media_filename(&dir_name);
            let show_title = if parsed_show.title.is_empty() {
                "Unknown Show".to_string()
            } else {
                parsed_show.title
            };

            // Find or create show using repository
            let show = match self.show_repo.find_by_title(&show_title).await? {
                Some(s) => s,
                None => {
                    self.show_repo
                        .create(CreateShow {
                            title: show_title.clone(),
                            year: parsed_show.year,
                        })
                        .await?
                }
            };

            // Ensure library-show association exists
            self.show_repo
                .ensure_library_association(lib_uuid, show.id)
                .await?;

            if let Some(enrichment_repo) = &self.enrichment_repo {
                enrichment_repo
                    .ensure_pending(beam_domain::models::enrichment::EnrichmentTargetId::Show(
                        show.id,
                    ))
                    .await?;
            }

            // Find or create season
            let season = self
                .show_repo
                .find_or_create_season(show.id, season_num)
                .await?;

            // Create episode
            let episode_title = if parsed.title.is_empty() {
                file_stem.to_string()
            } else {
                parsed.title
            };
            let create_episode = CreateEpisode {
                season_id: season.id,
                episode_number: episode_num,
                title: episode_title,
                runtime: Some(duration),
            };
            let episode = self.show_repo.create_episode(create_episode).await?;

            Ok(MediaFileContent::Episode {
                episode_id: episode.id,
            })
        } else {
            // IT IS A MOVIE
            let movie_title = if parsed.title.is_empty() {
                file_stem.to_string()
            } else {
                parsed.title
            };

            // Find or create movie using repository
            let movie = match self.movie_repo.find_by_title(&movie_title).await? {
                Some(m) => m,
                None => {
                    let create_movie = CreateMovie {
                        title: movie_title,
                        year: parsed.year,
                        runtime: Some(duration),
                    };
                    self.movie_repo.create(create_movie).await?
                }
            };

            // Ensure library-movie association exists
            self.movie_repo
                .ensure_library_association(lib_uuid, movie.id)
                .await?;

            if let Some(enrichment_repo) = &self.enrichment_repo {
                enrichment_repo
                    .ensure_pending(beam_domain::models::enrichment::EnrichmentTargetId::Movie(
                        movie.id,
                    ))
                    .await?;
            }

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

    /// Process a NEW file to add it to the library.
    async fn process_new_file(&self, path: &Path, lib_uuid: Uuid) -> Result<bool, IndexError> {
        info!("Processing new file: {}", path.display());

        let (size, mtime) = read_fs_meta(path).map_err(|e| {
            IndexError::PathNotFound(format!(
                "Failed to read metadata for {}: {}",
                path.display(),
                e
            ))
        })?;

        if !is_known_video(path) {
            // Unsupported extension: index as Unknown. Hash it (when enabled) so
            // duplicate detection still covers it.
            let hash = if self.hash_unknown_files {
                self.hash_service
                    .hash_async(path.to_path_buf())
                    .await
                    .unwrap_or(0)
            } else {
                0
            };
            let file = self
                .file_repo
                .create(CreateMediaFile {
                    library_id: lib_uuid,
                    path: path.to_path_buf(),
                    hash,
                    size_bytes: size,
                    mtime,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: FileStatus::Unknown,
                })
                .await?;
            self.check_and_report_duplicate(&file).await;
            self.check_and_report_runtime_divergence(&file).await;
            return Ok(true);
        }

        // Known video: extract metadata first.
        let metadata = match self.media_info_service.get_video_metadata(path).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to extract metadata for {}: {}", path.display(), e);
                self.file_repo
                    .create(CreateMediaFile {
                        library_id: lib_uuid,
                        path: path.to_path_buf(),
                        hash: 0,
                        size_bytes: size,
                        mtime,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: None,
                        status: FileStatus::Unknown,
                    })
                    .await?;
                return Ok(true);
            }
        };

        let hash = self
            .hash_service
            .hash_async(path.to_path_buf())
            .await
            .map_err(|e| {
                error!("Failed to hash file {}: {}", path.display(), e);
                IndexError::PathNotFound(format!("Hash failed: {}", e))
            })?;

        let duration = Duration::from_secs_f64(metadata.duration_seconds());
        let content = self
            .classify_media_content(path, lib_uuid, duration)
            .await?;

        let file = self
            .file_repo
            .create(CreateMediaFile {
                library_id: lib_uuid,
                path: path.to_path_buf(),
                hash,
                size_bytes: size,
                mtime,
                mime_type: Some(format!("video/{}", metadata.format_name)),
                duration: Some(duration),
                container_format: Some(metadata.format_name.clone()),
                content: Some(content),
                status: FileStatus::Known,
            })
            .await?;

        self.insert_media_streams(file.id, &metadata).await?;
        self.check_and_report_duplicate(&file).await;
        self.check_and_report_runtime_divergence(&file).await;
        Ok(true)
    }

    /// Reconcile a file already present in the index against its current state
    /// on disk. Shared by the full scan and single-path watcher events.
    async fn reconcile_existing_file(
        &self,
        existing: &MediaFile,
        path: &Path,
    ) -> Result<(), IndexError> {
        let (size, mtime) = match read_fs_meta(path) {
            Ok(m) => m,
            Err(e) => {
                // A transient stat failure must not delete or corrupt the row.
                warn!("Failed to stat {}: {}", path.display(), e);
                return Ok(());
            }
        };

        // Cheap gate: only a size or mtime change warrants a rehash.
        if size == existing.size_bytes && mtime == existing.mtime {
            record_file_outcome("unchanged");
            return Ok(());
        }

        let known_video = is_known_video(path);
        if !known_video && !self.hash_unknown_files {
            // Unsupported extension with hashing disabled: just record size/mtime.
            self.file_repo
                .update(UpdateMediaFile {
                    id: existing.id,
                    hash: None,
                    size_bytes: Some(size),
                    mtime,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: None,
                })
                .await?;
            record_file_outcome("changed");
            return Ok(());
        }

        // Rehash to confirm the content actually changed.
        let new_hash = match self.hash_service.hash_async(path.to_path_buf()).await {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to hash {}: {}", path.display(), e);
                record_file_outcome("failed");
                return Ok(());
            }
        };

        if new_hash == existing.hash {
            // Content unchanged (e.g. mtime bumped by `touch`): refresh size/mtime.
            self.file_repo
                .update(UpdateMediaFile {
                    id: existing.id,
                    hash: None,
                    size_bytes: Some(size),
                    mtime,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: None,
                })
                .await?;
            record_file_outcome("unchanged");
            return Ok(());
        }

        self.reconcile_changed_file(existing, path, size, mtime, new_hash, known_video)
            .await?;
        record_file_outcome("changed");
        Ok(())
    }

    /// Apply a confirmed content change: refresh hash, metadata and streams.
    /// The file's movie/episode classification is intentionally left unchanged
    /// since the path (and therefore the inferred title) has not moved.
    async fn reconcile_changed_file(
        &self,
        existing: &MediaFile,
        path: &Path,
        size: u64,
        mtime: Option<DateTime<Utc>>,
        new_hash: u64,
        known_video: bool,
    ) -> Result<(), IndexError> {
        info!("File content changed, reconciling: {}", path.display());

        if !known_video {
            let updated = self
                .file_repo
                .update(UpdateMediaFile {
                    id: existing.id,
                    hash: Some(new_hash),
                    size_bytes: Some(size),
                    mtime,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: Some(FileStatus::Unknown),
                })
                .await?;
            self.check_and_report_duplicate(&updated).await;
            self.check_and_report_runtime_divergence(&updated).await;
            return Ok(());
        }

        match self.media_info_service.get_video_metadata(path).await {
            Ok(metadata) => {
                // Replace the file's stream set with the freshly extracted one.
                self.stream_repo.delete_by_file_id(existing.id).await?;
                self.insert_media_streams(existing.id, &metadata).await?;

                let duration = Duration::from_secs_f64(metadata.duration_seconds());
                let updated = self
                    .file_repo
                    .update(UpdateMediaFile {
                        id: existing.id,
                        hash: Some(new_hash),
                        size_bytes: Some(size),
                        mtime,
                        mime_type: Some(format!("video/{}", metadata.format_name)),
                        duration: Some(duration),
                        container_format: Some(metadata.format_name.clone()),
                        content: None,
                        status: Some(FileStatus::Known),
                    })
                    .await?;
                self.check_and_report_duplicate(&updated).await;
                self.check_and_report_runtime_divergence(&updated).await;
                Ok(())
            }
            Err(e) => {
                warn!(
                    "Failed to re-extract metadata for changed file {}: {}",
                    path.display(),
                    e
                );
                let updated = self
                    .file_repo
                    .update(UpdateMediaFile {
                        id: existing.id,
                        hash: Some(new_hash),
                        size_bytes: Some(size),
                        mtime,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: None,
                        status: Some(FileStatus::Changed),
                    })
                    .await?;
                self.check_and_report_duplicate(&updated).await;
                self.check_and_report_runtime_divergence(&updated).await;
                Ok(())
            }
        }
    }

    /// Report files that share content (same XXH3 hash) with `file`.
    async fn check_and_report_duplicate(&self, file: &MediaFile) {
        if file.hash == 0 {
            return; // unhashed sentinel
        }
        let matches = match self.file_repo.find_by_hash(file.hash).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Duplicate check failed for {}: {}", file.path.display(), e);
                return;
            }
        };
        let duplicates: Vec<String> = matches
            .into_iter()
            .filter(|f| f.id != file.id)
            .map(|f| f.path.display().to_string())
            .collect();
        if duplicates.is_empty() {
            return;
        }

        let message = format!(
            "Duplicate content detected: '{}' shares its hash with {} other file(s)",
            file.path.display(),
            duplicates.len()
        );
        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            message.clone(),
            Some(file.library_id.to_string()),
            None,
        ));
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                message,
                Some(serde_json::json!({
                    "file": file.path.display().to_string(),
                    "duplicates": duplicates,
                })),
            )
            .await;
    }

    /// Warn when `file` maps to a movie/episode that already has other files
    /// whose probed runtime disagrees beyond [`DivergencePolicy`]'s thresholds
    /// -- usually a sign of a misnamed or mismatched file (issue #88). Never
    /// fails the scan: like [`Self::check_and_report_duplicate`], every
    /// repository error is swallowed with a `warn!` and the method returns.
    async fn check_and_report_runtime_divergence(&self, file: &MediaFile) {
        // Skip files without a usable probed duration -- nothing to compare.
        let Some(duration) = file.duration else {
            return;
        };
        let file_secs = duration.as_secs_f64();
        if file_secs <= 0.0 {
            return;
        }

        // Gather the sibling files that share this file's movie/episode.
        let siblings: Vec<MediaFile> = match &file.content {
            Some(MediaFileContent::Movie { movie_entry_id }) => {
                let entry = match self.movie_repo.find_entry_by_id(*movie_entry_id).await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => return,
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                };
                let entries = match self
                    .movie_repo
                    .find_entries_by_movie_id(entry.movie_id)
                    .await
                {
                    Ok(entries) => entries,
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                };
                let mut collected = Vec::new();
                for entry in entries {
                    match self.file_repo.find_by_movie_entry_id(entry.id).await {
                        Ok(files) => collected.extend(files),
                        Err(e) => {
                            warn!(
                                "Runtime-divergence check failed for {}: {}",
                                file.path.display(),
                                e
                            );
                            return;
                        }
                    }
                }
                collected
            }
            Some(MediaFileContent::Episode { episode_id }) => {
                match self.file_repo.find_by_episode_id(*episode_id).await {
                    Ok(files) => files,
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                }
            }
            None => return,
        };

        // Compare against every sibling that carries a probed runtime, keeping
        // the pairs that diverge past both thresholds.
        let policy = &self.divergence_policy;
        let mut diverging: Vec<(&MediaFile, f64)> = Vec::new();
        for sibling in &siblings {
            if sibling.id == file.id {
                continue;
            }
            let Some(sibling_duration) = sibling.duration else {
                continue;
            };
            let sibling_secs = sibling_duration.as_secs_f64();
            if sibling_secs <= 0.0 {
                continue;
            }
            let delta = (file_secs - sibling_secs).abs();
            let ratio = delta / file_secs.max(sibling_secs);
            if ratio > policy.max_runtime_ratio && delta > policy.min_runtime_delta_secs {
                diverging.push((sibling, sibling_secs));
            }
        }
        if diverging.is_empty() {
            return;
        }

        // Name the sibling that diverges most (largest absolute delta) in the
        // single warning we emit.
        let (worst_sibling, worst_secs) = diverging
            .iter()
            .max_by(|(_, a), (_, b)| (file_secs - *a).abs().total_cmp(&(file_secs - *b).abs()))
            .copied()
            .expect("diverging is non-empty");

        let message = format!(
            "Runtime mismatch: '{}' ({}) differs from '{}' ({}); {} rendition(s) of the same title \
             diverge -- likely a misnamed or mismatched file",
            file.path.display(),
            humanize_minutes(file_secs),
            worst_sibling.path.display(),
            humanize_minutes(worst_secs),
            diverging.len(),
        );

        self.notification_service.publish(AdminEvent::warning(
            EventCategory::LibraryScan,
            message.clone(),
            Some(file.library_id.to_string()),
            None,
        ));
        let siblings_json: Vec<serde_json::Value> = diverging
            .iter()
            .map(|(sibling, secs)| {
                serde_json::json!({
                    "path": sibling.path.display().to_string(),
                    "duration_secs": secs,
                })
            })
            .collect();
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::LibraryScan,
                message,
                Some(serde_json::json!({
                    "file": file.path.display().to_string(),
                    "siblings": siblings_json,
                    "threshold": {
                        "max_runtime_ratio": policy.max_runtime_ratio,
                        "min_runtime_delta_secs": policy.min_runtime_delta_secs,
                    },
                })),
            )
            .await;
        metrics::counter!("beam_index_divergence_warnings_total").increment(1);
    }

    /// Publish a warning for a file that could not be processed, without
    /// aborting the rest of the scan.
    async fn report_file_failure(
        &self,
        lib_uuid: Uuid,
        library_name: &str,
        path: &Path,
        err: &IndexError,
    ) {
        error!("Failed to process file {}: {}", path.display(), err);
        record_file_outcome("failed");
        self.notification_service.publish(AdminEvent::warning(
            EventCategory::LibraryScan,
            format!("Failed to process file '{}': {}", path.display(), err),
            Some(lib_uuid.to_string()),
            Some(library_name.to_string()),
        ));
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::LibraryScan,
                format!("Failed to process file: {}", path.display()),
                Some(serde_json::json!({
                    "library_id": lib_uuid.to_string(),
                    "path": path.display().to_string(),
                    "error": err.to_string(),
                })),
            )
            .await;
    }

    /// Scan every library. Used for the startup scan and the periodic backstop.
    /// A failure in one library is logged and does not abort the others.
    pub async fn scan_all_libraries(&self) -> Result<u32, IndexError> {
        let libraries = self.library_repo.find_all().await?;
        let mut total_added = 0;
        for library in libraries {
            match self.scan_library(library.id.to_string()).await {
                Ok(added) => total_added += added,
                Err(e) => error!("Scan failed for library {}: {}", library.id, e),
            }
        }
        Ok(total_added)
    }

    /// Reconcile a single path in response to a filesystem-watcher event.
    pub async fn reconcile_path(
        &self,
        library_id: Uuid,
        path: PathBuf,
        kind: FsEventKind,
    ) -> Result<(), IndexError> {
        // Ignore events for libraries that no longer exist.
        if self.library_repo.find_by_id(library_id).await?.is_none() {
            return Ok(());
        }

        let path_str = path.to_string_lossy().to_string();

        if kind == FsEventKind::Removed || !path.is_file() {
            if let Some(file) = self.file_repo.find_by_path(&path_str).await? {
                info!("Removing deleted file from index: {}", path.display());
                self.file_repo.delete(file.id).await?;
            }
            return Ok(());
        }

        match self.file_repo.find_by_path(&path_str).await? {
            Some(existing) => self.reconcile_existing_file(&existing, &path).await,
            None => {
                if self.process_new_file(&path, library_id).await? {
                    record_file_outcome("new");
                }
                Ok(())
            }
        }
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
        let mut existing_map: HashMap<PathBuf, beam_domain::models::MediaFile> = existing_files
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
                // Known file: reconcile against its current on-disk state.
                if let Err(e) = self.reconcile_existing_file(&existing_file, &path).await {
                    self.report_file_failure(lib_uuid, &library.name, &path, &e)
                        .await;
                }
            } else {
                // New file.
                match self.process_new_file(&path, lib_uuid).await {
                    Ok(true) => {
                        added_count += 1;
                        record_file_outcome("new");
                    }
                    Ok(false) => {}
                    Err(e) => {
                        self.report_file_failure(lib_uuid, &library.name, &path, &e)
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
    use crate::probe::color::{
        ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
        PixelFormat,
    };
    use crate::probe::format::{ChannelLayout, Disposition, SampleFormat};
    use crate::probe::media::{CodecId, Discard};
    use crate::probe::metadata::MetadataError;
    use crate::probe::metadata::StreamMetadata as UtilStreamMetadata;
    use crate::probe::metadata::{
        AudioMetadata, AudioStreamMetadata as UtilAudioStream,
        SubtitleStreamMetadata as UtilSubtitleStream, VideoFileMetadata, VideoMetadata,
        VideoStreamMetadata as UtilVideoStream,
    };
    use crate::services::admin_log::LocalAdminLogService;
    use crate::services::admin_log::NoOpAdminLogService;
    use crate::services::hash::MockHashService;
    use crate::services::media_info::MockMediaInfoService;
    use crate::services::notification::EventLevel;
    use crate::services::notification::InMemoryNotificationService;
    use beam_domain::models::{CreateLibrary, Library, MediaFile};
    use beam_domain::repositories::AdminLogRepository;
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use beam_domain::repositories::file::MockFileRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::library::MockLibraryRepository;
    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_domain::repositories::movie::MockMovieRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::MockShowRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::stream::MockMediaStreamRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use num::rational::Ratio;
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

    fn make_service_with_stream_repo(
        stream_repo: Arc<InMemoryMediaStreamRepository>,
    ) -> LocalIndexService {
        LocalIndexService::new(
            Arc::new(MockLibraryRepository::new()),
            Arc::new(MockFileRepository::new()),
            Arc::new(MockMovieRepository::new()),
            Arc::new(MockShowRepository::new()),
            stream_repo,
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        )
    }

    fn make_video_stream(
        index: usize,
        width: u32,
        height: u32,
        bit_rate: u64,
        codec_name: &str,
        frame_rate: Option<Ratio<i32>>,
    ) -> UtilStreamMetadata {
        UtilStreamMetadata::Video(UtilVideoStream {
            index,
            time_base: Ratio::new(1, 1000),
            start_time: 0,
            duration: 1_000_000,
            frames: 0,
            disposition: Disposition::default(),
            discard: Discard::Default,
            rate: frame_rate,
            codec_id: CodecId::H264,
            video: VideoMetadata {
                bit_rate,
                max_rate: 0,
                delay: 0,
                width,
                height,
                format: PixelFormat::None,
                has_b_frames: false,
                aspect_ratio: Ratio::new(16, 9),
                color_space: ColorSpace::BT709,
                color_range: ColorRange::Unspecified,
                color_primaries: ColorPrimaries::BT709,
                color_transfer_characteristic: ColorTransferCharacteristic::BT709,
                chroma_location: ChromaLocation::Unspecified,
                references: 0,
                intra_dc_precision: 0,
                profile: "Main".to_string(),
                level: "4.0".to_string(),
                codec_name: codec_name.to_string(),
            },
            metadata: std::collections::HashMap::new(),
        })
    }

    fn make_audio_stream(
        index: usize,
        language: &str,
        title: &str,
        channels: u16,
        sample_rate: u32,
        bit_rate: u64,
        codec_name: &str,
    ) -> UtilStreamMetadata {
        UtilStreamMetadata::Audio(UtilAudioStream {
            index,
            time_base: Ratio::new(1, 1000),
            start_time: 0,
            duration: 1_000_000,
            frames: 0,
            disposition: Disposition::default(),
            discard: Discard::Default,
            rate: None,
            codec_id: CodecId::AAC,
            audio: AudioMetadata {
                bit_rate,
                max_rate: 0,
                delay: 0,
                rate: sample_rate,
                channels,
                format: SampleFormat::None,
                frames: 0,
                align: 0,
                channel_layout: ChannelLayout {
                    channels,
                    description: None,
                },
                codec_name: codec_name.to_string(),
                profile: "LC".to_string(),
                title: title.to_string(),
                language: language.to_string(),
            },
            metadata: std::collections::HashMap::new(),
        })
    }

    fn make_subtitle_stream(
        index: usize,
        language: Option<&str>,
        title: Option<&str>,
    ) -> UtilStreamMetadata {
        let mut metadata = std::collections::HashMap::new();
        if let Some(lang) = language {
            metadata.insert("language".to_string(), lang.to_string());
        }
        if let Some(t) = title {
            metadata.insert("title".to_string(), t.to_string());
        }
        UtilStreamMetadata::Subtitle(UtilSubtitleStream {
            index,
            time_base: Ratio::new(1, 1000),
            start_time: 0,
            duration: 1_000_000,
            disposition: Disposition::default(),
            discard: Discard::Default,
            codec_id: CodecId::SUBRIP,
            metadata,
        })
    }

    /// Override a stream's disposition flags after construction, so the
    /// `make_*_stream` builders above don't need a `disposition` parameter
    /// threaded through every existing call site.
    fn with_disposition(
        mut stream: UtilStreamMetadata,
        default: bool,
        forced: bool,
    ) -> UtilStreamMetadata {
        let disposition = Disposition::for_test(default, forced);
        match &mut stream {
            UtilStreamMetadata::Video(v) => v.disposition = disposition,
            UtilStreamMetadata::Audio(a) => a.disposition = disposition,
            UtilStreamMetadata::Subtitle(s) => s.disposition = disposition,
        }
        stream
    }

    /// Override a video stream's color transfer characteristic after
    /// construction, for HDR-detection tests.
    fn with_transfer_characteristic(
        mut stream: UtilStreamMetadata,
        transfer: ColorTransferCharacteristic,
    ) -> UtilStreamMetadata {
        if let UtilStreamMetadata::Video(v) = &mut stream {
            v.video.color_transfer_characteristic = transfer;
        }
        stream
    }

    fn make_stream_file_metadata(streams: Vec<UtilStreamMetadata>) -> VideoFileMetadata {
        VideoFileMetadata {
            file_path: PathBuf::from("test.mp4"),
            metadata: Default::default(),
            best_video_stream: None,
            best_audio_stream: None,
            best_subtitle_stream: None,
            duration: 1_000_000,
            streams,
            format_name: "mp4".to_string(),
            format_long_name: "MPEG-4".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
        }
    }

    // ── insert_media_streams unit tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_insert_video_stream_fields() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_video_stream(
            0,
            1920,
            1080,
            5_000_000,
            "h264",
            Some(Ratio::new(30, 1)),
        )]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert_eq!(result.unwrap(), 1);

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert_eq!(streams.len(), 1);

        let s = &streams[0];
        assert_eq!(
            s.stream_type,
            beam_domain::models::stream::StreamType::Video
        );
        assert_eq!(s.codec, "h264");
        assert_eq!(s.index, 0);

        if let beam_domain::models::stream::StreamMetadata::Video(v) = &s.metadata {
            assert_eq!(v.width, 1920);
            assert_eq!(v.height, 1080);
            assert_eq!(v.frame_rate, Some(30.0));
            assert_eq!(v.bit_rate, Some(5_000_000));
        } else {
            panic!("expected Video metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_video_stream_persists_sdr_color_metadata() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_video_stream(
            0, 1920, 1080, 5_000_000, "h264", None,
        )]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
            assert_eq!(v.color_space.as_deref(), Some("BT.709"));
            assert_eq!(v.color_range.as_deref(), Some("Unspecified"));
            assert_eq!(v.hdr_format, None);
        } else {
            panic!("expected Video metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_video_stream_smpte2084_persists_hdr10() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let stream = make_video_stream(0, 3840, 2160, 20_000_000, "hevc", None);
        let stream = with_transfer_characteristic(stream, ColorTransferCharacteristic::SMPTE2084);
        let metadata = make_stream_file_metadata(vec![stream]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
            assert_eq!(v.hdr_format.as_deref(), Some("HDR10"));
        } else {
            panic!("expected Video metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_video_stream_arib_std_b67_persists_hlg() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let stream = make_video_stream(0, 3840, 2160, 20_000_000, "hevc", None);
        let stream = with_transfer_characteristic(stream, ColorTransferCharacteristic::AribStdB67);
        let metadata = make_stream_file_metadata(vec![stream]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
            assert_eq!(v.hdr_format.as_deref(), Some("HLG"));
        } else {
            panic!("expected Video metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_audio_stream_with_language() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_audio_stream(
            0, "eng", "English", 2, 48_000, 128_000, "aac",
        )]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert_eq!(result.unwrap(), 1);

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert_eq!(streams.len(), 1);

        let s = &streams[0];
        assert_eq!(
            s.stream_type,
            beam_domain::models::stream::StreamType::Audio
        );
        assert_eq!(s.codec, "aac");

        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &s.metadata {
            assert_eq!(a.language, Some("eng".to_string()));
        } else {
            panic!("expected Audio metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_audio_stream_empty_language_becomes_none() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_audio_stream(
            0, "", "", 2, 48_000, 128_000, "aac",
        )]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
            assert_eq!(a.language, None);
            assert_eq!(a.title, None);
        } else {
            panic!("expected Audio metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_audio_stream_title_populated_or_none() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![
            make_audio_stream(0, "eng", "Director Commentary", 2, 48_000, 128_000, "aac"),
            make_audio_stream(1, "eng", "", 2, 48_000, 128_000, "aac"),
        ]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert_eq!(streams.len(), 2);

        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
            assert_eq!(a.title, Some("Director Commentary".to_string()));
        } else {
            panic!("expected Audio metadata");
        }
        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[1].metadata {
            assert_eq!(a.title, None);
        } else {
            panic!("expected Audio metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_audio_stream_channels_and_sample_rate() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_audio_stream(
            0, "eng", "", 6, 48_000, 448_000, "ac3",
        )]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
            assert_eq!(a.channels, 6);
            assert_eq!(a.sample_rate, 48_000);
        } else {
            panic!("expected Audio metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_audio_stream_default_and_forced_flags_persisted() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let default_track = with_disposition(
            make_audio_stream(0, "eng", "", 2, 48_000, 128_000, "aac"),
            true,
            false,
        );
        let commentary_track = with_disposition(
            make_audio_stream(1, "eng", "Commentary", 2, 48_000, 128_000, "aac"),
            false,
            false,
        );
        let metadata = make_stream_file_metadata(vec![default_track, commentary_track]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
            assert!(a.is_default);
            assert!(!a.is_forced);
        } else {
            panic!("expected Audio metadata");
        }
        if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[1].metadata {
            assert!(!a.is_default);
        } else {
            panic!("expected Audio metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_subtitle_stream_default_and_forced_flags_persisted() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let forced_track = with_disposition(
            make_subtitle_stream(0, Some("eng"), Some("Forced")),
            false,
            true,
        );
        let metadata = make_stream_file_metadata(vec![forced_track]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        if let beam_domain::models::stream::StreamMetadata::Subtitle(sub) = &streams[0].metadata {
            assert!(!sub.is_default);
            assert!(sub.is_forced);
        } else {
            panic!("expected Subtitle metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_subtitle_stream_fields() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![make_subtitle_stream(
            0,
            Some("eng"),
            Some("English SDH"),
        )]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert_eq!(result.unwrap(), 1);

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert_eq!(streams.len(), 1);

        let s = &streams[0];
        assert_eq!(
            s.stream_type,
            beam_domain::models::stream::StreamType::Subtitle
        );

        if let beam_domain::models::stream::StreamMetadata::Subtitle(sub) = &s.metadata {
            assert_eq!(sub.language, Some("eng".to_string()));
            assert_eq!(sub.title, Some("English SDH".to_string()));
        } else {
            panic!("expected Subtitle metadata");
        }
    }

    #[tokio::test]
    async fn test_insert_mixed_streams_all_inserted() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![
            make_video_stream(0, 1920, 1080, 5_000_000, "h264", Some(Ratio::new(24, 1))),
            make_audio_stream(1, "eng", "English", 2, 48_000, 192_000, "aac"),
            make_audio_stream(2, "fra", "French", 2, 48_000, 128_000, "aac"),
            make_subtitle_stream(3, Some("eng"), Some("English")),
        ]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert_eq!(result.unwrap(), 4);

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert_eq!(streams.len(), 4);

        use beam_domain::models::stream::StreamType;
        assert_eq!(streams[0].stream_type, StreamType::Video);
        assert_eq!(streams[1].stream_type, StreamType::Audio);
        assert_eq!(streams[2].stream_type, StreamType::Audio);
        assert_eq!(streams[3].stream_type, StreamType::Subtitle);
    }

    #[tokio::test]
    async fn test_insert_empty_streams_returns_zero() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let metadata = make_stream_file_metadata(vec![]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert_eq!(result.unwrap(), 0);

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        assert!(streams.is_empty());
    }

    #[tokio::test]
    async fn test_insert_streams_db_error_propagates() {
        let mut mock_stream_repo = MockMediaStreamRepository::new();
        mock_stream_repo
            .expect_insert_streams()
            .times(1)
            .returning(|_| Err(sea_orm::DbErr::Custom("simulated DB failure".to_string())));

        let service = LocalIndexService::new(
            Arc::new(MockLibraryRepository::new()),
            Arc::new(MockFileRepository::new()),
            Arc::new(MockMovieRepository::new()),
            Arc::new(MockShowRepository::new()),
            Arc::new(mock_stream_repo),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let file_id = Uuid::new_v4();
        let metadata = make_stream_file_metadata(vec![make_video_stream(
            0, 1280, 720, 2_000_000, "h264", None,
        )]);

        let result = service.insert_media_streams(file_id, &metadata).await;
        assert!(matches!(result, Err(IndexError::Db(_))));
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
        assert_eq!(movies[0].title, "The Matrix Reloaded");
        assert_eq!(movies[0].year, Some(2003));
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
        assert_eq!(movies[0].title, "movie");
        assert_eq!(movies[0].year, Some(2024));
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
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fake movie data").unwrap();
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
                Ok(beam_domain::models::Movie {
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
                    anilist_id: None,
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
                Ok(beam_domain::models::MovieEntry {
                    id: entry_id,
                    library_id: Uuid::new_v4(),
                    movie_id: Uuid::new_v4(),
                    edition: None,
                    is_primary: true,
                    created_at: chrono::Utc::now(),
                })
            });

        mock_file_repo
            .expect_find_by_hash()
            .times(1)
            .returning(|_| Ok(vec![]));

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(beam_domain::models::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 12345,
                size_bytes: 1024,
                mtime: None,
                mime_type: Some("video/mp4".to_string()),
                duration: None,
                container_format: None,
                content: Some(beam_domain::models::MediaFileContent::Movie {
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
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fake episode data").unwrap();
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
            Ok(beam_domain::models::Show {
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
                anilist_id: None,
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
                Ok(beam_domain::models::Season {
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
                Ok(beam_domain::models::Episode {
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

        mock_file_repo
            .expect_find_by_hash()
            .times(1)
            .returning(|_| Ok(vec![]));

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(beam_domain::models::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 67890,
                size_bytes: 500 * 1024 * 1024,
                mtime: None,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: None,
                content: Some(beam_domain::models::MediaFileContent::Episode { episode_id }),
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

    // ============================
    // SCAN LIBRARY INTEGRATION TESTS
    // ============================

    fn make_video_metadata() -> VideoFileMetadata {
        VideoFileMetadata {
            file_path: PathBuf::from("test"),
            metadata: std::collections::HashMap::default(),
            best_video_stream: None,
            best_audio_stream: None,
            best_subtitle_stream: None,
            duration: 1_000_000,
            streams: vec![],
            format_name: "mp4".to_string(),
            format_long_name: "MPEG-4".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
        }
    }

    async fn make_library_in_tempdir(
        lib_repo: &InMemoryLibraryRepository,
        dir: &TempDir,
    ) -> Library {
        lib_repo
            .create(CreateLibrary {
                name: "Test Library".to_string(),
                root_path: dir.path().to_path_buf(),
                description: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_scan_library_empty() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 0);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_scan_library_new_video_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"fake video content").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(12345));

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Known);
    }

    #[tokio::test]
    async fn test_scan_library_new_non_video_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("notes.txt");
        std::fs::write(&file_path, b"some text content").unwrap();

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        )
        .with_hash_unknown_files(false);

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
    }

    #[tokio::test]
    async fn test_scan_library_multiple_new_files() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        for name in &["alpha.mkv", "beta.mkv", "gamma.mkv"] {
            std::fs::write(dir.path().join(name), b"fake video").unwrap();
        }

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(3)
            .returning(|_| Ok(99999));

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(3)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 3);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    async fn test_scan_library_changed_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // A real video file on disk (16 bytes).
        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"new content size").unwrap();

        // Seed the DB with the same path but a stale hash/size, so the scan
        // detects the content change and reconciles it.
        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 12345,
            size_bytes: 999,
            mtime: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing.clone());

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(99999));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 0); // no new files added

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        // The changed file was re-hashed, re-extracted and is healthy again.
        assert_eq!(files[0].status, FileStatus::Known);
        assert_eq!(files[0].size_bytes, 16);
        assert_eq!(files[0].hash, 99999);
    }

    #[tokio::test]
    async fn test_scan_library_removed_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // Seed the file repo with a phantom file that doesn't exist on disk
        let phantom = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: dir.path().join("ghost.mp4"),
            hash: 0,
            size_bytes: 1024,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(phantom.id, phantom.clone());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 0); // no new files

        // Phantom record must have been deleted
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_scan_library_invalid_root_path() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());

        // Insert a library whose root_path does not exist on disk
        let library = Library {
            id: Uuid::new_v4(),
            name: "Bad Library".to_string(),
            root_path: PathBuf::from("/tmp/beam-nonexistent-xyzzy-12345"),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        };
        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(library.id, library.clone());

        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            admin_log_svc,
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert!(matches!(result, Err(IndexError::PathNotFound(_))));

        // An error-level notification must have been published
        let events = notification_svc.published_events();
        assert!(events.iter().any(|e| {
            matches!(e.level, EventLevel::Error) && matches!(e.category, EventCategory::LibraryScan)
        }));

        // Admin log must also record an error-level LibraryScan entry
        let logs = admin_log_repo.list(10, 0).await.unwrap();
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
        }));
    }

    #[tokio::test]
    async fn test_scan_library_media_extraction_failure() {
        // When media-info extraction fails, process_new_file still inserts the file
        // with Unknown status and returns Ok(true), so added_count is incremented.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("corrupt.mp4");
        std::fs::write(&file_path, b"not real video data").unwrap();

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".to_string())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
    }

    #[tokio::test]
    async fn test_scan_library_process_failure_sends_warning() {
        // When process_new_file returns Err (e.g. hash fails), scan_library
        // publishes a warning notification and continues rather than aborting.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("problem.mp4");
        std::fs::write(&file_path, b"video data").unwrap();

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Err(std::io::Error::other("hash io error")));

        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            notification_svc.clone(),
            admin_log_svc,
        );

        // Scan should succeed overall; the failing file is not counted
        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 0);

        // A warning notification should have been published for the failed file
        let events = notification_svc.published_events();
        assert!(events.iter().any(|e| {
            matches!(e.level, EventLevel::Warning)
                && matches!(e.category, EventCategory::LibraryScan)
        }));

        // Admin log must also have a warning entry mentioning the failed file path
        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let file_path_str = file_path.display().to_string();
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Warning
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains(&file_path_str)
        }));

        // The file must not have been added to the repo
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_scan_library_updates_timestamps() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        assert!(library.last_scan_started_at.is_none());
        assert!(library.last_scan_finished_at.is_none());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let updated = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(updated.last_scan_started_at.is_some());
        assert!(updated.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_admin_log_and_notifications() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            admin_log_svc,
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        // At least one Info notification with LibraryScan category whose message names the library
        let events = notification_svc.published_events();
        assert!(events.iter().any(|e| {
            matches!(e.level, EventLevel::Info)
                && matches!(e.category, EventCategory::LibraryScan)
                && e.message.contains("Test Library")
        }));

        // Admin log must have a "scan started" entry
        let logs = admin_log_repo.list(10, 0).await.unwrap();
        assert!(!logs.is_empty());
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Info
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains("scan started")
        }));

        // Admin log must have a "scan completed" entry
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Info
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains("scan completed")
        }));
    }

    #[tokio::test]
    async fn test_scan_publishes_correct_event_counts() {
        // Seed: 2 pre-existing DB records, 1 matching file on disk and 1 phantom.
        // Disk: 1 matching file (stays) + 1 phantom (removed) + 1 brand-new file (added).
        // Expected: added=1, removed=1 in the admin-log completion entry.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // File A: exists in DB and on disk with the same size → stays unchanged
        let stays_path = dir.path().join("stays.txt");
        std::fs::write(&stays_path, b"hello").unwrap(); // 5 bytes
        let file_a = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: stays_path.clone(),
            hash: 0,
            size_bytes: 5,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);

        // File B: exists in DB only (phantom, no matching disk file) → will be removed
        let phantom_path = dir.path().join("phantom.txt");
        let file_b = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: phantom_path,
            hash: 0,
            size_bytes: 100,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        // File C: exists on disk only (not in DB) → will be added as Unknown (non-video)
        let new_path = dir.path().join("new_file.txt");
        std::fs::write(&new_path, b"new").unwrap();

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_svc,
        )
        .with_hash_unknown_files(false);

        let added = service.scan_library(library.id.to_string()).await.unwrap();
        assert_eq!(added, 1);

        // Admin log completion entry must record added=1, removed=1 in its JSON details
        let logs = admin_log_repo.list(100, 0).await.unwrap();
        let completion = logs
            .iter()
            .find(|l| l.message.contains("scan completed"))
            .expect("expected a 'scan completed' admin log entry");
        let details = completion
            .details
            .as_ref()
            .expect("completion log has JSON details");
        assert_eq!(details["added"], serde_json::json!(1));
        assert_eq!(details["removed"], serde_json::json!(1));
    }

    // ─── reconcile, dedup, reconcile_path, scan_all_libraries ───────────────

    #[tokio::test]
    async fn test_reconcile_unchanged_file_skips_rehash() {
        // A file whose size AND mtime match the DB record must not be rehashed.
        // MockHashService::new() has no expectation, so any hash call would panic.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"unchanged content").unwrap();
        let disk_meta = std::fs::metadata(&file_path).unwrap();
        let mtime: Option<DateTime<Utc>> = disk_meta.modified().ok().map(|t| t.into());

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 4242,
            size_bytes: disk_meta.len(),
            mtime,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hash, 4242, "hash must not have been rewritten");
        assert_eq!(files[0].status, FileStatus::Known);
    }

    #[tokio::test]
    async fn test_reconcile_same_hash_touches_mtime_only() {
        // Suspected change (mtime differs) but rehash matches → only mtime is
        // refreshed; no ffmpeg call, no status change.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"same content as hash").unwrap();
        let disk_meta = std::fs::metadata(&file_path).unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 8888,
            size_bytes: disk_meta.len(), // size matches
            mtime: None,                 // stale → suspected
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(8888));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(MockMediaInfoService::new()), // no expectation: ffmpeg must NOT be called
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hash, 8888);
        assert_eq!(files[0].status, FileStatus::Known);
        assert!(files[0].mtime.is_some(), "mtime was refreshed");
    }

    #[tokio::test]
    async fn test_reconcile_changed_file_ffmpeg_failure_marks_changed() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"new content").unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 100,
            size_bytes: 999, // wrong size → suspected
            mtime: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(200)); // differs from existing.hash
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".into())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Changed);
        assert_eq!(files[0].hash, 200);
    }

    #[tokio::test]
    async fn test_reconcile_path_removed_deletes_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let ghost_path = dir.path().join("ghost.mp4");
        // The file is intentionally NOT created on disk.

        let phantom = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: ghost_path.clone(),
            hash: 0,
            size_bytes: 10,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
        };
        file_repo.files.lock().unwrap().insert(phantom.id, phantom);

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service
            .reconcile_path(library.id, ghost_path, FsEventKind::Removed)
            .await
            .unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "removed file must be deleted from index");
    }

    #[tokio::test]
    async fn test_reconcile_path_creates_new_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("new.mp4");
        std::fs::write(&file_path, b"fresh video").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash.expect_hash_async().times(1).returning(|_| Ok(42));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service
            .reconcile_path(library.id, file_path.clone(), FsEventKind::Created)
            .await
            .unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, file_path);
        assert_eq!(files[0].hash, 42);
    }

    #[tokio::test]
    async fn test_reconcile_path_unknown_library_is_noop() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        // No library matches this id; reconcile_path must be a no-op.
        service
            .reconcile_path(
                Uuid::new_v4(),
                PathBuf::from("/nonexistent/path.mp4"),
                FsEventKind::Created,
            )
            .await
            .unwrap();

        assert!(file_repo.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unknown_file_hashed_when_enabled() {
        // hash_unknown_files defaults to true, so even a .txt file is hashed
        // for duplicate detection. Status still ends up Unknown.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("notes.txt");
        std::fs::write(&file_path, b"text").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(555));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
        assert_eq!(files[0].hash, 555);
    }

    #[tokio::test]
    async fn test_duplicate_detection_logs() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // Two .mp4 files that the mock hash service deliberately hashes to the
        // same value, exercising the dedup-on-create path.
        std::fs::write(dir.path().join("first.mp4"), b"one").unwrap();
        std::fs::write(dir.path().join("second.mp4"), b"two").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(2)
            .returning(|_| Ok(777));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(2)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_svc,
        );

        service.scan_library(library.id.to_string()).await.unwrap();

        let logs = admin_log_repo.list(100, 0).await.unwrap();
        assert!(
            logs.iter().any(|l| {
                l.level == AdminLogLevel::Info
                    && l.category == AdminLogCategory::LibraryScan
                    && l.message.contains("Duplicate")
            }),
            "an admin log entry must flag the duplicate"
        );
    }

    #[tokio::test]
    async fn test_scan_all_libraries_sums_added_counts() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let _ = make_library_in_tempdir(&lib_repo, &dir_a).await;
        let _ = make_library_in_tempdir(&lib_repo, &dir_b).await;
        std::fs::write(dir_a.path().join("a.mp4"), b"video a").unwrap();
        std::fs::write(dir_b.path().join("b.mp4"), b"video b").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(2)
            .returning(|_| Ok(1234));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(2)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let total = service.scan_all_libraries().await.unwrap();
        assert_eq!(total, 2);
    }

    // ─── runtime-divergence detection (issue #88) ──────────────────────────────

    /// Build a service wired for divergence tests: real in-memory file + movie
    /// repos (so sibling lookups resolve), the caller's notification + admin-log
    /// services for inspection, and mocks for the parts a bare divergence check
    /// never touches.
    fn make_divergence_service(
        file_repo: Arc<dyn FileRepository>,
        movie_repo: Arc<dyn MovieRepository>,
        notification: Arc<InMemoryNotificationService>,
        admin_log: Arc<dyn AdminLogService>,
    ) -> LocalIndexService {
        LocalIndexService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            file_repo,
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification,
            admin_log,
        )
    }

    fn make_file_with_content(
        content: Option<MediaFileContent>,
        duration_secs: Option<f64>,
        path: &str,
    ) -> MediaFile {
        MediaFile {
            id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            path: PathBuf::from(path),
            hash: 0,
            size_bytes: 1024,
            mtime: None,
            mime_type: None,
            duration: duration_secs.map(Duration::from_secs_f64),
            container_format: None,
            content,
            status: FileStatus::Known,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Seed a movie with two entries (one file each) under the same movie id and
    /// return the service, the notification fake, the admin-log repo, and the
    /// first file (the one that gets checked).
    async fn seed_two_movie_renditions(
        first_secs: Option<f64>,
        second_secs: Option<f64>,
    ) -> (
        LocalIndexService,
        Arc<InMemoryNotificationService>,
        Arc<InMemoryAdminLogRepository>,
        MediaFile,
    ) {
        use beam_domain::models::{CreateMovie, CreateMovieEntry};

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));

        let library_id = Uuid::new_v4();
        let movie = movie_repo
            .create(CreateMovie {
                title: "Some Movie".to_string(),
                year: None,
                runtime: None,
            })
            .await
            .unwrap();
        let entry_a = movie_repo
            .create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: None,
                is_primary: true,
            })
            .await
            .unwrap();
        let entry_b = movie_repo
            .create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: Some("Extended".to_string()),
                is_primary: false,
            })
            .await
            .unwrap();

        let file_a = make_file_with_content(
            Some(MediaFileContent::Movie {
                movie_entry_id: entry_a.id,
            }),
            first_secs,
            "/media/movie-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::Movie {
                movie_entry_id: entry_b.id,
            }),
            second_secs,
            "/media/movie-b.mkv",
        );
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file_a.id, file_a.clone());
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        let service =
            make_divergence_service(file_repo, movie_repo, notification.clone(), admin_log_svc);
        (service, notification, admin_log_repo, file_a)
    }

    #[tokio::test]
    async fn test_divergence_movie_wildly_different_runtimes_warns() {
        // 40 min vs 90 min: ratio ~0.56 and delta 3000s both blow past the
        // thresholds → exactly one warning across both admin channels.
        let (service, notification, admin_log_repo, file_a) =
            seed_two_movie_renditions(Some(40.0 * 60.0), Some(90.0 * 60.0)).await;

        service.check_and_report_runtime_divergence(&file_a).await;

        let warnings: Vec<_> = notification
            .published_events()
            .into_iter()
            .filter(|e| matches!(e.level, EventLevel::Warning))
            .collect();
        assert_eq!(warnings.len(), 1, "expected exactly one warning event");
        let warning = &warnings[0];
        assert!(matches!(warning.category, EventCategory::LibraryScan));
        assert!(warning.message.contains("/media/movie-a.mkv"));
        assert!(warning.message.contains("/media/movie-b.mkv"));
        assert!(warning.message.contains("40 min"));
        assert!(warning.message.contains("90 min"));

        // The durable admin log must also carry a Warning LibraryScan entry.
        let logs = admin_log_repo.list(100, 0).await.unwrap();
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Warning
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains("Runtime mismatch")
        }));
    }

    #[tokio::test]
    async fn test_divergence_within_threshold_no_warning() {
        // 90 min vs 92 min: delta 120s < 240s (and ratio ~0.022 < 0.15) → quiet.
        let (service, notification, _admin_log_repo, file_a) =
            seed_two_movie_renditions(Some(90.0 * 60.0), Some(92.0 * 60.0)).await;

        service.check_and_report_runtime_divergence(&file_a).await;

        assert!(
            notification.published_events().is_empty(),
            "renditions within threshold must not warn"
        );
    }

    #[tokio::test]
    async fn test_divergence_short_content_guard_no_warning() {
        // Two episodes at 2 min vs 3 min: the ratio (0.33) clears the relative
        // threshold, but the 60s delta is far under the 240s floor → no warning.
        let episode_id = Uuid::new_v4();
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());

        let file_a = make_file_with_content(
            Some(MediaFileContent::Episode { episode_id }),
            Some(2.0 * 60.0),
            "/media/ep-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::Episode { episode_id }),
            Some(3.0 * 60.0),
            "/media/ep-b.mkv",
        );
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file_a.id, file_a.clone());
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        let service = make_divergence_service(
            file_repo,
            Arc::new(InMemoryMovieRepository::default()),
            notification.clone(),
            Arc::new(NoOpAdminLogService),
        );

        service.check_and_report_runtime_divergence(&file_a).await;

        assert!(
            notification.published_events().is_empty(),
            "short content below the absolute floor must not warn"
        );
    }

    #[tokio::test]
    async fn test_divergence_episode_siblings_warn() {
        // 30 min vs 60 min episodes: both thresholds cleared → one warning.
        let episode_id = Uuid::new_v4();
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());

        let file_a = make_file_with_content(
            Some(MediaFileContent::Episode { episode_id }),
            Some(30.0 * 60.0),
            "/media/ep-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::Episode { episode_id }),
            Some(60.0 * 60.0),
            "/media/ep-b.mkv",
        );
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file_a.id, file_a.clone());
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        let service = make_divergence_service(
            file_repo,
            Arc::new(InMemoryMovieRepository::default()),
            notification.clone(),
            Arc::new(NoOpAdminLogService),
        );

        service.check_and_report_runtime_divergence(&file_a).await;

        let warnings: Vec<_> = notification
            .published_events()
            .into_iter()
            .filter(|e| matches!(e.level, EventLevel::Warning))
            .collect();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("/media/ep-b.mkv"));
    }

    #[tokio::test]
    async fn test_divergence_unprobed_sibling_skipped() {
        // The checked file is probed (40 min) but its only sibling has no
        // duration → the sibling is skipped and nothing is flagged.
        let (service, notification, _admin_log_repo, file_a) =
            seed_two_movie_renditions(Some(40.0 * 60.0), None).await;

        service.check_and_report_runtime_divergence(&file_a).await;

        assert!(
            notification.published_events().is_empty(),
            "an unprobed sibling must be skipped, not flagged"
        );
    }

    #[tokio::test]
    async fn test_divergence_check_never_fails_on_repo_error() {
        // A repository error during the sibling lookup must be swallowed: the
        // check returns without publishing anything, so it can never abort a
        // scan (it is invoked with `.await`, never `?`).
        let mut mock_file = MockFileRepository::new();
        mock_file
            .expect_find_by_episode_id()
            .times(1)
            .returning(|_| Err(sea_orm::DbErr::Custom("simulated lookup failure".into())));

        let notification = Arc::new(InMemoryNotificationService::new());
        let service = make_divergence_service(
            Arc::new(mock_file),
            Arc::new(MockMovieRepository::new()),
            notification.clone(),
            Arc::new(NoOpAdminLogService),
        );

        let file = make_file_with_content(
            Some(MediaFileContent::Episode {
                episode_id: Uuid::new_v4(),
            }),
            Some(45.0 * 60.0),
            "/media/only.mkv",
        );

        // Must complete (infallible) and publish nothing.
        service.check_and_report_runtime_divergence(&file).await;
        assert!(notification.published_events().is_empty());
    }
}
