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
use beam_domain::services::{Clock, RealClock};

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

/// What a walk of a library root found on disk.
///
/// A named result rather than a bare `Vec` so the walk can report more than
/// the files it reached without changing its call site.
struct WalkOutcome {
    /// Every regular file under the root, in walk order. Non-video files are
    /// included: the scan indexes them as `Unknown`.
    files: Vec<PathBuf>,
    /// How many of `files` have a known video extension -- the files Beam can
    /// index as media, and so the ones the empty-root guard counts.
    video_files: usize,
    /// Every path the walk failed to read: a directory it could not list, or
    /// a listed entry it could not stat for any reason other than "not
    /// found". The walk says nothing about what is beneath one of these, so an
    /// indexed row under it is left exactly as it is rather than marked
    /// missing (issue #179).
    failed_subtrees: Vec<PathBuf>,
    /// Whether the walk hit an error it could not attribute to a path. Nothing
    /// then scopes what the walk failed to see, so no row is marked missing.
    unscoped_failure: bool,
}

/// Walks a library root and collects every regular file beneath it.
///
/// An entry the walk cannot read is collected as a failure rather than
/// dropped: a subdirectory that fails to list, or a listed file that fails to
/// stat, contributes no files, and
/// reading that silence as "every file under it is gone" is how a transient
/// permission or I/O error used to delete rows.
fn walk_library_root(root: &Path) -> WalkOutcome {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut failed_subtrees: Vec<PathBuf> = Vec::new();
    let mut unscoped_failure = false;
    for entry in WalkDir::new(root) {
        match entry {
            Ok(entry) => {
                // A directory is descended by the walk itself, which reports
                // any failure to list it as an `Err` below.
                if entry.file_type().is_dir() {
                    continue;
                }
                let path = entry.into_path();
                // `metadata` follows a symlink, as `Path::is_file` did, so a
                // link to a regular file is still indexed. Only a stat that
                // says "no such file" (a dangling link, a file deleted
                // mid-walk) means the entry is absent. Any other failure --
                // a listable but unsearchable parent (EACCES), a transient
                // EIO or ESTALE on a network mount -- says nothing about the
                // file, so it shields the path exactly like a directory the
                // walk could not list (issue #179).
                match std::fs::metadata(&path) {
                    Ok(meta) => {
                        if meta.is_file() {
                            files.push(path);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        warn!(path = %path.display(), error = %err, "library walk could not stat a listed entry");
                        failed_subtrees.push(path);
                    }
                }
            }
            Err(err) => match err.path() {
                Some(path) => {
                    warn!(path = %path.display(), error = %err, "library walk could not read a path");
                    failed_subtrees.push(path.to_path_buf());
                }
                None => {
                    warn!(error = %err, "library walk failed without a path");
                    unscoped_failure = true;
                }
            },
        }
    }
    let video_files = files.iter().filter(|path| is_known_video(path)).count();
    WalkOutcome {
        files,
        video_files,
        failed_subtrees,
        unscoped_failure,
    }
}

/// What to do with the indexed rows a scan's walk did not see.
#[derive(Debug, Default, PartialEq, Eq)]
struct MissingPlan {
    /// Rows to stamp `missing_since = now`: not yet marked, and not under a
    /// path the walk failed to read.
    ///
    /// With a zero grace period a row can be in both `mark` and `purge`.
    /// [`FileRepository::purge_missing`] only removes rows that are already
    /// stamped, so the scan marks before it purges.
    mark: Vec<Uuid>,
    /// Rows missing for at least the grace period, to purge.
    purge: Vec<Uuid>,
    /// How many unseen rows were left untouched because the walk could not
    /// vouch for their absence.
    shielded: usize,
}

/// Decide the fate of every row a walk did not see.
///
/// Pure so the grace arithmetic and the walk-error shielding are tested as a
/// table rather than through a filesystem that has to be made to fail: a walk
/// error is the one input a `TempDir` cannot reliably produce (CLAUDE.md,
/// "no `FileSystem` trait").
///
/// A row is *shielded* -- left exactly as it is -- when the walk hit an error
/// it could not scope, or when the row's path lies under a path the walk
/// failed to read. `Path::starts_with` compares whole components, so a
/// failure at `/a/b` shields `/a/b/c.mkv` but not `/a/bc.mkv`. Every other
/// row is missing as of its first stamp (or `now`, if this scan is the first
/// to notice) and is purged once `now - missing_since` reaches `grace`.
fn plan_missing<'a>(
    unseen: impl IntoIterator<Item = &'a MediaFile>,
    failed_subtrees: &[PathBuf],
    unscoped_failure: bool,
    now: DateTime<Utc>,
    grace: Duration,
) -> MissingPlan {
    let grace = chrono::TimeDelta::from_std(grace).unwrap_or(chrono::TimeDelta::MAX);
    let mut plan = MissingPlan::default();
    for file in unseen {
        let shielded = unscoped_failure
            || failed_subtrees
                .iter()
                .any(|failed| file.path.starts_with(failed));
        if shielded {
            plan.shielded += 1;
            continue;
        }
        if file.missing_since.is_none() {
            plan.mark.push(file.id);
        }
        let since = file.missing_since.unwrap_or(now);
        if now - since >= grace {
            plan.purge.push(file.id);
        }
    }
    plan
}

/// The most failed paths an admin-log entry lists; the count is always exact.
const MAX_REPORTED_FAILED_PATHS: usize = 50;

/// How long a file may stay missing before a scan purges its row, unless the
/// caller sets it with [`LocalIndexService::with_missing_file_grace`].
pub const DEFAULT_MISSING_FILE_GRACE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

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
    /// The message never contains a filesystem path (NFR-108). The admin
    /// boundary relies on that unconditionally, so every site constructing this
    /// variant must uphold it; `assert_names_no_path` pins all four.
    ///
    /// The four sites are the scan's two root guards (a root that is not a
    /// directory, and a root that holds no video files while the library has
    /// indexed video files) and the two `process_new_file` failures (metadata,
    /// hash).
    ///
    /// The path itself goes to a structured `tracing` field at the failing
    /// site. Where it *also* reaches the admin log differs by site, and the
    /// difference is the caller's, not this variant's: the root-guard sites
    /// write their own admin-log record, while the two `process_new_file` sites
    /// are logged later by `report_file_failure` — which runs on the scan path
    /// only. Reached through `reconcile_path`, a per-file failure is logged by
    /// `beam-index`'s runtime and never reaches the admin log at all.
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
    clock: Arc<dyn Clock>,
    missing_file_grace: Duration,
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
            clock: Arc::new(RealClock),
            missing_file_grace: DEFAULT_MISSING_FILE_GRACE,
        }
    }

    /// Override the clock that stamps scan times and `missing_since`, and that
    /// the grace period is measured against. Defaults to [`RealClock`].
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Override how long a file may stay missing from disk before a scan
    /// purges its row -- and, through `ON DELETE CASCADE`, its playback
    /// progress (issue #179). Zero purges at the first healthy scan that does
    /// not find the file. Defaults to [`DEFAULT_MISSING_FILE_GRACE`].
    pub fn with_missing_file_grace(mut self, grace: Duration) -> Self {
        self.missing_file_grace = grace;
        self
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
            warn!(path = %path.display(), error = %e, "Failed to read file metadata");
            IndexError::PathNotFound(format!("Could not read file metadata: {e}"))
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
                error!(path = %path.display(), error = %e, "Failed to hash file");
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

    /// Clear `missing_since` on a row whose path is back on disk, returning
    /// whether it had been missing. The row keeps its id, so its playback
    /// progress is still attached (issue #179). Shared by the full scan and
    /// single-path watcher events.
    async fn restore_if_missing(&self, existing: &MediaFile) -> Result<bool, IndexError> {
        if existing.missing_since.is_none() {
            return Ok(false);
        }
        info!(
            "Missing file is back, restoring: {}",
            existing.path.display()
        );
        self.file_repo.restore(existing.id).await?;
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

    /// Tell the operator that part of a library could not be read, and that
    /// the rows beneath it were left as they were rather than marked missing.
    async fn report_walk_failures(
        &self,
        lib_uuid: Uuid,
        library_name: &str,
        failed_subtrees: &[PathBuf],
        unscoped_failure: bool,
        shielded: usize,
    ) {
        warn!(
            library_id = %lib_uuid,
            failed_paths = failed_subtrees.len(),
            unscoped_failure,
            shielded,
            "library walk could not read part of the root; rows beneath were left untouched"
        );
        self.notification_service.publish(AdminEvent::warning(
            EventCategory::LibraryScan,
            format!(
                "Library '{}': {} path(s) could not be read during the scan; {} indexed file(s) \
                 beneath them were left as they were",
                library_name,
                failed_subtrees.len(),
                shielded
            ),
            Some(lib_uuid.to_string()),
            Some(library_name.to_string()),
        ));
        let reported: Vec<String> = failed_subtrees
            .iter()
            .take(MAX_REPORTED_FAILED_PATHS)
            .map(|path| path.display().to_string())
            .collect();
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::LibraryScan,
                format!(
                    "Library scan could not read {} path(s) in \"{}\"; {} indexed file(s) left untouched",
                    failed_subtrees.len(),
                    library_name,
                    shielded
                ),
                Some(serde_json::json!({
                    "library_id": lib_uuid.to_string(),
                    "failed_path_count": failed_subtrees.len(),
                    "failed_paths": reported,
                    "unscoped_failure": unscoped_failure,
                    "shielded": shielded,
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
        let Some(library) = self.library_repo.find_by_id(library_id).await? else {
            return Ok(());
        };

        let path_str = path.to_string_lossy().to_string();

        // Only a stat that says "no such file" means the path is gone. Any
        // other failure (EACCES from an unsearchable parent, a transient EIO
        // or ESTALE on a network mount) says nothing about the file, so the
        // event is dropped rather than read as a deletion; the next scan
        // shields the same path (issue #179).
        let is_file = if kind == FsEventKind::Removed {
            false
        } else {
            match std::fs::metadata(&path) {
                Ok(meta) => meta.is_file(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
                Err(err) => {
                    warn!(
                        path = %path.display(),
                        error = %err,
                        "could not stat a changed path; leaving its row as it is"
                    );
                    return Ok(());
                }
            }
        };

        if !is_file {
            // A root that is not there is a volume that went away, not a file
            // that was deleted: say nothing about the file until the next scan
            // can see the root again (issue #179).
            if !library.root_path.is_dir() {
                warn!(
                    path = %path.display(),
                    root = %library.root_path.display(),
                    "library root is unavailable; ignoring the removal"
                );
                return Ok(());
            }
            if let Some(file) = self.file_repo.find_by_path(&path_str).await?
                && file.missing_since.is_none()
            {
                info!("Marking deleted file missing: {}", path.display());
                self.file_repo
                    .mark_missing(vec![file.id], self.clock.now())
                    .await?;
            }
            return Ok(());
        }

        match self.file_repo.find_by_path(&path_str).await? {
            Some(existing) => {
                self.restore_if_missing(&existing).await?;
                self.reconcile_existing_file(&existing, &path).await
            }
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
        let start_time = self.clock.now();

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

        // A refused scan never stamps `last_scan_started_at`: the start time is
        // written only once every guard below has passed, so a refusal cannot
        // leave the library reading as "scanning" with no finish to follow.
        if !library.root_path.is_dir() {
            warn!(
                root = %library.root_path.display(),
                library_id = %lib_uuid,
                "library root is not a directory"
            );
            self.notification_service.publish(AdminEvent::error(
                EventCategory::LibraryScan,
                format!(
                    "Library '{}' root path does not exist or is not a directory: {}",
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
                        "Library scan failed: root path does not exist or is not a directory for \"{}\"",
                        library.name
                    ),
                    Some(serde_json::json!({
                        "library_id": library_id,
                        "path": library.root_path
                    })),
                )
                .await;
            return Err(IndexError::PathNotFound(
                "Library root path does not exist or is not a directory".to_string(),
            ));
        }

        // Phase 1: Fetch existing files from DB -- missing ones included, so a
        // path that comes back is matched to its old row (and id), and so the
        // empty-root guard below still counts a row that is already missing.
        let existing_files = self
            .file_repo
            .find_all_by_library_including_missing(lib_uuid)
            .await?;
        let mut existing_map: HashMap<PathBuf, beam_domain::models::MediaFile> = existing_files
            .into_iter()
            .map(|f| (f.path.clone(), f))
            .collect();

        info!("Found {} existing files in DB", existing_map.len());

        // Phase 2: Walk FS
        let WalkOutcome {
            files: walked_files,
            video_files: walked_video_files,
            failed_subtrees,
            unscoped_failure,
        } = walk_library_root(&library.root_path);

        // An unmounted volume usually leaves its mount point behind as an empty
        // directory, which passes the guard above -- or one holding only a
        // sentinel or hidden file (`.not_mounted`, `.DS_Store`, `Thumbs.db`),
        // which admins put on mount points on purpose. Reconciling that walk
        // would mark every indexed video row missing, so a root with no video files
        // under a library that has indexed video files is refused rather than
        // believed. Only video rows are counted on either side: they are what
        // is at stake, and a library that only ever held non-video files has
        // nothing an unmounted volume could take from it. Emptying a library on
        // purpose is deleting the library.
        let indexed_video_files = existing_map
            .keys()
            .filter(|path| is_known_video(path))
            .count();
        if walked_video_files == 0 && indexed_video_files > 0 {
            warn!(
                root = %library.root_path.display(),
                library_id = %lib_uuid,
                indexed_video_files,
                "library root contains no video files but video files are indexed; refusing to reconcile"
            );
            self.notification_service.publish(AdminEvent::error(
                EventCategory::LibraryScan,
                format!(
                    "Library '{}' root contains no video files but {} are indexed; refusing to \
                     reconcile — is the volume mounted? Root: {}",
                    library.name,
                    indexed_video_files,
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
                        "Library scan refused: root contains no video files but {} are indexed for \"{}\"",
                        indexed_video_files, library.name
                    ),
                    Some(serde_json::json!({
                        "library_id": library_id,
                        "path": library.root_path,
                        "indexed_video_files": indexed_video_files,
                    })),
                )
                .await;
            return Err(IndexError::PathNotFound(
                "Library root contains no video files but video files are indexed; refusing to \
                 reconcile"
                    .to_string(),
            ));
        }

        // Update scan start time
        self.library_repo
            .update_scan_progress(lib_uuid, Some(start_time), None, None)
            .await?;

        let mut added_count = 0;
        let mut restored_count = 0u64;

        // Phase 3: Compare with DB, add new files
        for path in walked_files {
            if let Some(existing_file) = existing_map.remove(&path) {
                // Known file: bring it back if it was missing, then reconcile
                // it against its current on-disk state.
                let reconciled = match self.restore_if_missing(&existing_file).await {
                    Ok(restored) => {
                        if restored {
                            restored_count += 1;
                        }
                        self.reconcile_existing_file(&existing_file, &path).await
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = reconciled {
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

        // Phase 4: Soft-delete the rows the walk did not see, and purge the
        // ones that have been missing for the whole grace period. Nothing is
        // deleted on first sight: a file that is gone now may only be on a
        // volume that is away for a moment (issue #179).
        let now = self.clock.now();
        let plan = plan_missing(
            existing_map.values(),
            &failed_subtrees,
            unscoped_failure,
            now,
            self.missing_file_grace,
        );
        let MissingPlan {
            mark,
            purge,
            shielded,
        } = plan;
        let marked_count = self.file_repo.mark_missing(mark, now).await?;
        let purged_count = self.file_repo.purge_missing(purge).await?;
        if marked_count > 0 {
            info!("Marked {} files missing from library", marked_count);
        }
        if !failed_subtrees.is_empty() || unscoped_failure {
            self.report_walk_failures(
                lib_uuid,
                &library.name,
                &failed_subtrees,
                unscoped_failure,
                shielded,
            )
            .await;
        }
        if purged_count > 0 {
            info!(
                "Purged {} files missing for longer than the grace period",
                purged_count
            );
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Info,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "Purged {} files from \"{}\" that were missing for longer than the grace period",
                        purged_count, library.name
                    ),
                    Some(serde_json::json!({
                        "library_id": library_id,
                        "purged": purged_count,
                        "grace_secs": self.missing_file_grace.as_secs(),
                    })),
                )
                .await;
        }

        // Update scan finish time
        let end_time = self.clock.now();
        let total_files = self.library_repo.count_files(lib_uuid).await?;

        self.library_repo
            .update_scan_progress(lib_uuid, None, Some(end_time), Some(total_files as i32))
            .await?;

        info!(
            "Scan complete. Added: {}, Marked missing: {}, Restored: {}, Purged: {}, Total: {}",
            added_count, marked_count, restored_count, purged_count, total_files
        );

        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            format!(
                "Library scan complete for '{}': added {}, missing {}, restored {}, purged {}, total {}",
                library.name, added_count, marked_count, restored_count, purged_count, total_files
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
                    "Library scan completed: \"{}\" — {} added, {} marked missing, {} restored, {} purged, {} total",
                    library.name, added_count, marked_count, restored_count, purged_count, total_files
                ),
                Some(serde_json::json!({
                    "library_id": library_id,
                    "added": added_count,
                    "marked_missing": marked_count,
                    "restored": restored_count,
                    "purged": purged_count,
                    "total": total_files,
                })),
            )
            .await;

        Ok(added_count)
    }
}

#[cfg(test)]
#[path = "index_missing_tests.rs"]
mod missing_tests;

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

    /// `beam-server` passes an `IndexError::PathNotFound` message straight
    /// through as the `detail` of a 400 an administrator's browser renders, so
    /// the guarantee written on the variant has to hold at *every* construction
    /// site (NFR-108). The path reaches the operator through the `tracing`
    /// field, the admin notification and the admin log instead.
    fn assert_names_no_path(err: &IndexError, paths: &[&Path]) {
        let message = err.to_string();
        for path in paths {
            let path = path.to_string_lossy();
            assert!(
                !message.contains(path.as_ref()),
                "a client-facing rejection must not carry a filesystem path; \
                 {message:?} names {path:?}"
            );
        }
        assert!(
            !message.contains(std::path::MAIN_SEPARATOR),
            "a client-facing rejection must not carry any path component: {message:?}"
        );
    }

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
                missing_since: None,
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
                missing_since: None,
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
    async fn test_process_file_missing_path_reports_no_filesystem_path() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("movies/Vanished (2020).mkv");
        // Nothing is created: metadata cannot be read, so the file is refused
        // before any repository, hash, or probe call.
        let service = LocalIndexService::new(
            Arc::new(MockLibraryRepository::new()),
            Arc::new(MockFileRepository::new()),
            Arc::new(MockMovieRepository::new()),
            Arc::new(MockShowRepository::new()),
            Arc::new(MockMediaStreamRepository::new()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let err = service
            .process_new_file(&path, Uuid::new_v4())
            .await
            .expect_err("a file that cannot be stat'ed must be refused");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[&path]);
    }

    #[tokio::test]
    async fn test_process_file_hash_failure_reports_no_filesystem_path() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("Vanished (2020).mkv");
        std::fs::write(&path, b"video data").unwrap();

        // The error the real hasher yields when the file goes away between the
        // walk and the hash: an OS error from opening that very path. Whether
        // its `Display` carries the path is precisely what the guarantee on
        // `IndexError::PathNotFound` rests on at this construction site, so the
        // error has to be a real one rather than a hand-written string.
        let io_error = std::fs::File::open(temp_dir.path().join("Vanished (2020).mkv.part"))
            .expect_err("that file was never created");

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .return_once(move |_| Err(io_error));

        let service = LocalIndexService::new(
            Arc::new(MockLibraryRepository::new()),
            Arc::new(MockFileRepository::new()),
            Arc::new(MockMovieRepository::new()),
            Arc::new(MockShowRepository::new()),
            Arc::new(MockMediaStreamRepository::new()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let err = service
            .process_new_file(&path, Uuid::new_v4())
            .await
            .expect_err("a file that cannot be hashed must be refused");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[&path]);
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

        // Nothing was indexed, so there is nothing to lose: a new, empty
        // library scans to completion rather than being refused.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_started_at.is_some());
        assert_eq!(stored.last_scan_file_count, Some(0));
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
            missing_since: None,
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
    async fn test_scan_library_removed_file_is_marked_missing_not_deleted() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // A file that is still on disk and unchanged since it was indexed, so
        // the root is not empty and the scan reconciles rather than refuses.
        let kept_path = dir.path().join("kept.mp4");
        std::fs::write(&kept_path, b"still here").unwrap();
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

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
            missing_since: None,
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

        // The phantom is hidden from the visible reads but kept, stamped
        // missing, so its id -- and anything keyed on it -- survives (#179).
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        let ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![kept.id]);
        let stored = file_repo
            .find_by_path(&phantom.path.to_string_lossy())
            .await
            .unwrap()
            .expect("a missing file is soft-deleted, not deleted");
        assert!(stored.missing_since.is_some());
    }

    /// The row a previous scan would have written for a file that is on disk
    /// and unchanged, so reconciling it takes the size-and-mtime fast path and
    /// touches neither the hasher nor the prober.
    fn indexed_file_matching_disk(library_id: Uuid, path: &Path) -> MediaFile {
        let (size_bytes, mtime) = read_fs_meta(path).unwrap();
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: path.to_path_buf(),
            hash: 0,
            size_bytes,
            mtime,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    /// A row for a file indexed under `root` by an earlier scan and not on
    /// disk now.
    fn indexed_file_under(library_id: Uuid, root: &Path, name: &str) -> MediaFile {
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: root.join(name),
            hash: 42,
            size_bytes: 1024,
            mtime: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    /// A scan service over `root` for a library that already has `indexed`
    /// rows, with the admin log and notifications captured.
    struct IndexedLibraryHarness {
        library: Library,
        lib_repo: Arc<InMemoryLibraryRepository>,
        file_repo: Arc<InMemoryFileRepository>,
        notification_svc: Arc<InMemoryNotificationService>,
        admin_log_repo: Arc<InMemoryAdminLogRepository>,
        service: LocalIndexService,
    }

    async fn indexed_library_harness(root: &Path, indexed: &[&str]) -> IndexedLibraryHarness {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let library = lib_repo
            .create(CreateLibrary {
                name: "Mounted Library".to_string(),
                root_path: root.to_path_buf(),
                description: None,
            })
            .await
            .unwrap();
        for name in indexed {
            let row = indexed_file_under(library.id, root, name);
            file_repo.files.lock().unwrap().insert(row.id, row);
        }
        // No expectations: reaching the hasher or the prober would be a bug.
        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
        );
        IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            notification_svc,
            admin_log_repo,
            service,
        }
    }

    #[tokio::test]
    async fn test_scan_library_empty_root_with_indexed_files_is_refused() {
        // An unmounted volume: the mount point is left behind as an empty
        // directory while the catalogue still holds the library's files.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            notification_svc,
            admin_log_repo,
            service,
        } = indexed_library_harness(root, &["a.mp4", "b.mkv"]).await;

        let err = service
            .scan_library(library.id.to_string())
            .await
            .expect_err("an empty root under indexed files must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[root]);

        // Nothing was reconciled away.
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 2, "a refused scan must not delete any row");

        // A refused scan never started, so the library must not read as
        // scanning.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
        assert_eq!(stored.last_scan_finished_at, None);

        // The operator is told, with the root and the count at stake.
        let root_str = root.to_string_lossy();
        let errors: Vec<_> = notification_svc
            .published_events()
            .into_iter()
            .filter(|e| {
                matches!(e.level, EventLevel::Error)
                    && matches!(e.category, EventCategory::LibraryScan)
            })
            .collect();
        assert_eq!(errors.len(), 1, "exactly one error event: {errors:?}");
        assert!(
            errors[0].message.contains(root_str.as_ref()),
            "the notification must name the refused root: {:?}",
            errors[0].message
        );
        assert!(
            errors[0].message.contains("2 are indexed"),
            "the notification must say how many rows were at stake: {:?}",
            errors[0].message
        );

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused scan must write an error-level LibraryScan admin-log entry");
        let details = entry
            .details
            .as_ref()
            .expect("the admin-log entry must carry a details payload");
        assert_eq!(
            details.get("path").and_then(|p| p.as_str()),
            Some(root_str.as_ref())
        );
        assert_eq!(
            details.get("indexed_video_files").and_then(|n| n.as_u64()),
            Some(2)
        );
        assert_eq!(
            details.get("library_id").and_then(|id| id.as_str()),
            Some(library.id.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn test_scan_library_root_with_only_empty_subdirs_is_refused() {
        // A root that is itself a directory of mount points, none of which is
        // mounted: directories, but not one file anywhere beneath them.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("disk1")).unwrap();
        std::fs::create_dir_all(root.join("disk2").join("Movies")).unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["disk1/a.mp4", "disk2/Movies/b.mkv"]).await;

        let err = service
            .scan_library(library.id.to_string())
            .await
            .expect_err("a root holding only empty directories must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 2, "a refused scan must not delete any row");
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
    }

    #[tokio::test]
    async fn test_scan_library_root_with_only_sentinel_files_is_refused() {
        // An unmounted volume whose mount point carries the files admins and
        // desktop OSes leave there on purpose: regular files, but not one
        // that Beam can index as media.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".not_mounted"), b"").unwrap();
        std::fs::write(root.join(".DS_Store"), b"\0").unwrap();
        std::fs::create_dir_all(root.join("Movies")).unwrap();
        std::fs::write(root.join("Movies").join("Thumbs.db"), b"\0").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            admin_log_repo,
            service,
            ..
        } = indexed_library_harness(root, &["a.mp4", "Movies/b.mkv"]).await;
        let before: Vec<Uuid> = {
            let mut ids: Vec<Uuid> = file_repo
                .find_all_by_library(library.id)
                .await
                .unwrap()
                .iter()
                .map(|f| f.id)
                .collect();
            ids.sort();
            ids
        };

        let err = service
            .scan_library(library.id.to_string())
            .await
            .expect_err("a root holding only non-video files must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[root]);

        // Every row survives, and the sentinels were not indexed either.
        let mut after: Vec<Uuid> = file_repo
            .find_all_by_library(library.id)
            .await
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        after.sort();
        assert_eq!(after, before, "a refused scan must not touch any row");
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused scan must write an error-level LibraryScan admin-log entry");
        assert_eq!(
            entry
                .details
                .as_ref()
                .and_then(|d| d.get("indexed_video_files"))
                .and_then(|n| n.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn test_scan_library_without_indexed_video_files_is_not_refused() {
        // The guard protects indexed video rows. A library that has only ever
        // held non-video files has none, so a walk finding no video files is
        // believed and a non-video file that went is marked missing as usual.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let kept_path = root.join("notes.nfo");
        std::fs::write(&kept_path, b"still here").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["gone.txt"]).await;
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

        assert_eq!(
            service.scan_library(library.id.to_string()).await.unwrap(),
            0
        );

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        let ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![kept.id]);
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_one_file_left_still_reconciles() {
        // The guard is for a walk that found no video files. A root that still
        // holds one is believed: the rows for the files that went are marked
        // missing.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let kept_path = root.join("kept.mp4");
        std::fs::write(&kept_path, b"still here").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["gone.mp4"]).await;
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

        assert_eq!(
            service.scan_library(library.id.to_string()).await.unwrap(),
            0
        );

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        let ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![kept.id]);
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_started_at.is_some());
        assert!(stored.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_invalid_root_path() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());

        // Insert a library whose root_path does not exist on disk. Derived from
        // a `TempDir` rather than a hardcoded `/tmp` name so the test does not
        // rest on an assumption about the host's filesystem: the parent is real
        // and this run owns it, and the child is guaranteed absent.
        let dir = TempDir::new().unwrap();
        let root_path = dir.path().join("beam-nonexistent-root");
        let library = Library {
            id: Uuid::new_v4(),
            name: "Bad Library".to_string(),
            root_path: root_path.clone(),
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
        let err = result.expect_err("a missing root must fail the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        // The path reaches the operator through the notification and admin
        // log below, never through the error message (NFR-108).
        assert_names_no_path(&err, &[&root_path]);

        // The compensating disclosure, asserted rather than assumed. Taking the
        // path out of the client-facing message is only safe because it reaches
        // the operator here instead, so this is the half of NFR-108's bargain
        // that has to be pinned: without it, the message could stay dutifully
        // path-free while the path reached nobody at all.
        let root = root_path.to_string_lossy();

        let events = notification_svc.published_events();
        let notification = events
            .iter()
            .find(|e| {
                matches!(e.level, EventLevel::Error)
                    && matches!(e.category, EventCategory::LibraryScan)
            })
            .expect("a refused root must publish an error-level LibraryScan event");
        assert!(
            notification.message.contains(root.as_ref()),
            "the notification is where the operator reads the root the scan refused; \
             {:?} does not name {root:?}",
            notification.message
        );

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused root must write an error-level LibraryScan admin-log entry");
        let details = entry
            .details
            .as_ref()
            .expect("the admin-log entry must carry a details payload");
        assert_eq!(
            details.get("path").and_then(|p| p.as_str()),
            Some(root.as_ref()),
            "the admin-log payload is the operator's other route to the root: {details:?}"
        );
    }

    #[tokio::test]
    async fn test_scan_library_root_is_a_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        // The root exists, but as a regular file: there is nothing to walk.
        let root_path = dir.path().join("movies.mkv");
        std::fs::write(&root_path, b"not a directory").unwrap();
        let library = lib_repo
            .create(CreateLibrary {
                name: "File Root".to_string(),
                root_path: root_path.clone(),
                description: None,
            })
            .await
            .unwrap();

        // No expectations: reaching the hasher or the prober would be a bug.
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

        let err = service
            .scan_library(library.id.to_string())
            .await
            .expect_err("a root that is not a directory must fail the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[&root_path]);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "nothing may be indexed under a file root");

        // A refused scan never started, so the library must not read as
        // scanning with no finish to follow.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
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
    async fn test_scan_library_real_prober_marks_corrupt_file_unknown() {
        // Same Unknown-on-unprobeable behaviour as above, but exercised through
        // the REAL FFmpeg prober (LocalMediaInfoService) rather than a stub:
        // a corrupt .mp4 makes `from_path` return Err, and the scan must
        // absorb it (file inserted as Unknown, scan still Ok) rather than abort.
        let _ = crate::probe::init();

        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("corrupt.mp4");
        std::fs::write(&file_path, b"not a real mp4 container at all").unwrap();

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            // Extraction fails before hashing, so the hash service is never hit.
            Arc::new(MockHashService::new()),
            Arc::new(crate::services::media_info::LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.scan_library(library.id.to_string()).await;
        assert_eq!(result.unwrap(), 1, "scan must succeed despite corrupt file");

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
        // Disk: 1 matching file (stays) + 1 phantom (marked missing) + 1 brand-new file (added).
        // Expected: added=1, marked_missing=1 in the admin-log completion entry.
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
            missing_since: None,
        };
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);

        // File B: exists in DB only (phantom, no matching disk file) → will be marked missing
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
            missing_since: None,
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

        // Admin log completion entry must record the counts in its JSON details
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
        assert_eq!(details["marked_missing"], serde_json::json!(1));
        assert_eq!(details["restored"], serde_json::json!(0));
        assert_eq!(details["purged"], serde_json::json!(0));
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
            missing_since: None,
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
            missing_since: None,
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
            missing_since: None,
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
    async fn test_reconcile_path_removed_marks_file_missing() {
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
            missing_since: None,
        };
        let phantom_id = phantom.id;
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
        assert!(files.is_empty(), "a removed file is hidden from the index");
        let stored = file_repo
            .find_all_by_library_including_missing(library.id)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1, "a removal never hard-deletes");
        assert_eq!(stored[0].id, phantom_id);
        assert!(stored[0].missing_since.is_some());
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
            missing_since: None,
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
