use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::disc::{self, DiscRead};
use crate::library_file::{
    EntryKind, FileMeta, LibraryFile, StatCursor, is_refusal, stat_regular_file,
};
use crate::probe::metadata::{StreamMetadata, VideoFileMetadata};
use crate::services::admin_log::AdminLogService;
use crate::services::filesystem_probe::{FilesystemKind, FilesystemProbe, StatfsFilesystemProbe};
use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;
use crate::services::mime::container_mime_type;
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use crate::services::scan::{
    CANCELLED, CatalogExclusive, ProgressThrottle, SCAN_STOP_TIMEOUT, ScanCoordinator, ScanEvent,
    ScanJob, ScanPhase, ScanProgress, ScanRefused, ScanState, ScanTicket, ScanTrigger, Settle,
    StoppedScan, settle_state,
};
use crate::services::watcher::FsEventKind;
use beam_domain::models::Library;
use beam_domain::models::admin_log::{AdminLogCategory, AdminLogLevel};
use beam_domain::models::chapter::Chapter;
use beam_domain::models::enrichment::{EnrichmentTargetId, FieldLocks, MetadataField};
use beam_domain::models::file::{
    CreateMediaFile, FileClassification, FileIdentity, FileRelink, FileStatus, MediaFile,
    MediaFileContent, ProbeUpdate, UpdateMediaFile, displaced_from, mtime_as_stored,
};
use beam_domain::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};
use beam_domain::models::show::{CreateEpisode, CreateShow, Episode, Show};
use beam_domain::models::watch_state::{TitleRef, WatchTarget};
use beam_domain::models::{PinSource, ProviderPin};
use beam_domain::repositories::{
    AppliedNfoRepository, ChapterRepository, EnrichmentStateRepository, FileRepository,
    LibraryRepository, MediaStreamRepository, MovieRepository, ShowRepository,
    SidecarSubtitleRepository, UserListRepository, WatchStateRepository,
};
use beam_domain::services::{Clock, IdGenerator, RealClock, UuidGenerator};
use beam_domain::utils::classification::{Classification, ContainerTags, Hints, classify};
use beam_domain::utils::disc::{
    DiscKind, DiscSetPlace, disc_set_member, disc_set_place, disc_stream_kind,
};
use beam_domain::utils::filename::{ParsedFilename, parse_media_filename};
use beam_domain::utils::identity::title_identity_key;
use beam_domain::utils::media_path::{
    CLASSIFIER_VERSION, EpisodeInference, MediaInference, MovieInference, TitleGuess,
    UnclassifiableReason, infer_media, season_folder_number,
};
use beam_domain::utils::path_policy::{PathDisposition, PathPolicy, is_video_path};

/// Read the size and modification time of `path`, a file beneath the
/// library `root`, the mtime as [`stored_mtime`] reads it. The stat follows
/// no link beneath the root ([`stat_regular_file`], issue #238), so a file
/// reached through a link swapped in since the walk fails rather than
/// lending its own.
fn read_fs_meta(root: &Path, path: &Path) -> std::io::Result<(u64, Option<DateTime<Utc>>)> {
    let meta = stat_regular_file(root, path)?;
    Ok((meta.size(), stored_mtime(&meta)))
}

/// Whether a library's filesystem keeps a file's inode number from one scan
/// to the next -- whether the inode half of a [`FileIdentity`] can be
/// compared at all.
///
/// A local filesystem does. A network or FUSE one may not: SMB mounted with
/// `noserverino`, and FUSE filesystems such as rclone, sshfs and mergerfs,
/// can hand out new inode numbers after a remount or a cache eviction, and
/// comparing them would make every file look changed -- and be hashed -- on
/// every scan. Such a library is compared by size, mtime and ctime. The
/// classification is the watcher's ([`FilesystemKind`]), so the libraries it
/// polls are exactly the ones whose inodes are not trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inodes {
    /// Inode numbers are compared.
    Stable,
    /// Inode numbers are recorded but never compared.
    Unstable,
}

impl From<FilesystemKind> for Inodes {
    fn from(kind: FilesystemKind) -> Self {
        match kind {
            FilesystemKind::Local => Inodes::Stable,
            FilesystemKind::Network => Inodes::Unstable,
        }
    }
}

/// What one stat says about a media file that a row records: its size,
/// modification time and [`FileIdentity`], each at the precision a row
/// keeps, and whether the inode it read can be compared ([`Inodes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStat {
    size: u64,
    mtime: Option<DateTime<Utc>>,
    identity: Option<FileIdentity>,
    inodes: Inodes,
}

impl FileStat {
    /// What `meta`, a regular file's, says, on a filesystem whose inodes
    /// are `inodes`.
    fn of(meta: &FileMeta, inodes: Inodes) -> Self {
        FileStat {
            size: meta.size(),
            mtime: stored_mtime(meta),
            identity: stored_identity(meta),
            inodes,
        }
    }

    /// Whether `row` records the file this stat describes, as far as a stat
    /// can tell -- so the file need not be hashed. See [`Self::agrees_with`].
    fn is_recorded_by(&self, row: &MediaFile) -> bool {
        self.agrees_with(row.size_bytes, row.mtime, row.identity)
    }

    /// Whether `earlier`, another stat of the same path, found the file as
    /// this one does: it did not change in between.
    fn is_same_as(&self, earlier: &FileStat) -> bool {
        self.agrees_with(earlier.size, earlier.mtime, earlier.identity)
    }

    /// The single "unchanged" rule. The size and mtime must be `size` and
    /// `mtime`, and the identity must be `identity` when both sides have one
    /// (issue #228): a swap or a rotation of files of one size and mtime
    /// changes every path's inode and ctime. Where inodes are
    /// [`Inodes::Unstable`], only the ctime of an identity is compared. An
    /// identity recorded before identities were, or a platform without them,
    /// falls back to size and mtime alone.
    fn agrees_with(
        &self,
        size: u64,
        mtime: Option<DateTime<Utc>>,
        identity: Option<FileIdentity>,
    ) -> bool {
        let identity_agrees = match (self.identity, identity) {
            (Some(found), Some(recorded)) => match self.inodes {
                Inodes::Stable => found == recorded,
                Inodes::Unstable => found.ctime == recorded.ctime,
            },
            _ => true,
        };
        self.size == size && self.mtime == mtime && identity_agrees
    }
}

/// Stat `path`, a file beneath the library `root`, for what a row records
/// of it (see [`FileStat`]), on a filesystem whose inodes are `inodes`. The
/// stat follows no link beneath the root ([`stat_regular_file`]): a link at
/// the file or at a folder above it fails ([`is_refusal`]) rather than
/// lending the stat of the file it leads to (issue #238).
fn read_stat(root: &Path, path: &Path, inodes: Inodes) -> std::io::Result<FileStat> {
    Ok(FileStat::of(&stat_regular_file(root, path)?, inodes))
}

/// Stats the files beneath one library root as a row records them
/// ([`FileStat`]), on a filesystem whose inodes are `inodes`: with no link
/// followed beneath the root and without opening a file ([`StatCursor`]).
/// One is kept for a pass over many files -- a scan's -- so each folder is
/// opened once rather than once per file beneath it.
struct LibraryStats {
    cursor: StatCursor,
    inodes: Inodes,
}

impl LibraryStats {
    fn new(root: &Path, inodes: Inodes) -> Self {
        LibraryStats {
            cursor: StatCursor::new(root),
            inodes,
        }
    }

    /// Stat `path`, a file beneath the root; see [`StatCursor::stat`].
    fn stat(&mut self, path: &Path) -> std::io::Result<FileStat> {
        Ok(FileStat::of(&self.cursor.stat(path)?, self.inodes))
    }

    /// What the library's filesystem keeps of a file's inode number.
    fn inodes(&self) -> Inodes {
        self.inodes
    }
}

/// The [`FileIdentity`] `meta` records, its ctime at the precision a row
/// keeps. `None` off Unix: Beam's first-class platforms are Linux and macOS,
/// and elsewhere a file is compared by size and mtime alone.
fn stored_identity(meta: &FileMeta) -> Option<FileIdentity> {
    #[cfg(unix)]
    {
        let (secs, nanos) = meta.ctime();
        let ctime = DateTime::from_timestamp(secs, nanos)?;
        Some(
            FileIdentity {
                inode: meta.ino(),
                ctime,
            }
            .as_stored(),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// The modification time `meta` records -- the only way the indexer reads an
/// mtime, for a media file or a sidecar. It comes back at the precision a row
/// keeps ([`mtime_as_stored`]), so it compares equal to the row of an
/// unchanged file (issue #229).
fn stored_mtime(meta: &FileMeta) -> Option<DateTime<Utc>> {
    meta.modified().map(|t| mtime_as_stored(t.into()))
}

/// The container tags a probe read, as classification takes them (issue #184).
fn container_tags(metadata: &VideoFileMetadata) -> ContainerTags {
    ContainerTags::from_tags(
        metadata
            .metadata
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    )
}

/// The most failed paths an admin-log entry lists; the count is always exact.
const MAX_REPORTED_FAILED_PATHS: usize = 50;

/// How long a file may stay missing before a scan purges its row, unless the
/// caller sets it with [`LocalIndexService::with_missing_file_grace`].
pub const DEFAULT_MISSING_FILE_GRACE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// `path` relative to the library `root` -- what the path policy and path
/// inference read. A path outside the root (never produced by the walk) is
/// returned whole.
fn relative_to<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

/// Records one processed-file outcome on the
/// `beam_index_files_processed_total{result}` counter, covering both full
/// scans and watcher-driven reconciles. `result` is one of `new`, `relinked`,
/// `changed`, `unchanged`, or `failed`. A no-op unless beam-server installed a metrics
/// recorder (`BEAM_ENABLE_METRICS=true`).
fn record_file_outcome(result: &'static str) {
    metrics::counter!("beam_index_files_processed_total", "result" => result).increment(1);
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("Database error: {0}")]
    Db(#[from] DbErr),
    #[error("Library not found")]
    LibraryNotFound,
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
    /// A scan job is already queued or running for the library.
    #[error("A scan of this library is already queued or running")]
    ScanInProgress,
    /// The scan was cancelled -- its library is being deleted.
    #[error("The scan was cancelled")]
    Cancelled,
}

impl IndexError {
    /// What a failed scan job reports: never a filesystem path (NFR-108),
    /// and never a database error's text, which can quote a row.
    fn job_failure(&self) -> String {
        match self {
            IndexError::PathNotFound(message) => message.clone(),
            IndexError::LibraryNotFound => "Library not found".to_string(),
            IndexError::Cancelled => CANCELLED.to_string(),
            IndexError::Db(_) | IndexError::ScanInProgress => "internal error".to_string(),
        }
    }
}

impl From<ScanRefused> for IndexError {
    fn from(refused: ScanRefused) -> Self {
        match refused {
            ScanRefused::InProgress => IndexError::ScanInProgress,
            // Being deleted: as good as gone to anyone asking to scan it.
            ScanRefused::Retired => IndexError::LibraryNotFound,
        }
    }
}

/// What became of a watcher event handed to
/// [`LocalIndexService::reconcile_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Reconciled, or nothing to do.
    Done,
    /// Not now: the library is being scanned, or the file is still being
    /// written. Hand the event back after `retry_after`.
    Deferred { retry_after: Duration },
}

/// How long a watcher event waits before it retries a library a scan holds.
pub const LIBRARY_BUSY_RETRY: Duration = Duration::from_secs(5);

/// How often a running scan publishes a progress event at most (FR-208). The
/// job itself is updated after every file.
pub const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_secs(1);

/// What one visit to one file came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    Added,
    /// A moved or renamed file, found at its new path and pointed to by its
    /// old row (issue #180).
    Relinked,
    Changed,
    Unchanged,
    /// Still being written: left alone, to be visited again after the
    /// duration.
    Deferred(Duration),
    /// Its content matches another file of the library that may have moved
    /// or been swapped with it: left as it is for the next scan, which sees
    /// every path at once, rather than guessed at by a watcher event (issue
    /// #180).
    LeftToScan,
    Failed,
}

/// The scan side of the indexer, as the server drives it.
///
/// A scan is two steps so the caller can answer before the scan runs:
/// [`Self::begin_scan`] checks the library and registers a job -- refusing
/// with [`IndexError::ScanInProgress`] while one is queued or running -- and
/// [`Self::run_scan`] runs it, typically on a task of its own.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait IndexService: Send + Sync + std::fmt::Debug {
    /// Register a scan of `library_id`. Fails with
    /// [`IndexError::LibraryNotFound`], with [`IndexError::PathNotFound`] when
    /// the library's root is not a directory (the message names no path), or
    /// with [`IndexError::ScanInProgress`].
    async fn begin_scan(
        &self,
        library_id: Uuid,
        trigger: ScanTrigger,
    ) -> Result<ScanTicket, IndexError>;

    /// Run a registered scan to the end, returning its final progress. The
    /// job reads `queued` until the library's lock is free.
    async fn run_scan(&self, ticket: ScanTicket) -> Result<ScanProgress, IndexError>;

    /// The latest scan job of `library_id` in this process, if any.
    fn scan_job(&self, library_id: Uuid) -> Option<ScanJob>;

    /// Follow `library_id`'s scan jobs.
    fn subscribe_scan(&self, library_id: Uuid) -> tokio::sync::watch::Receiver<Option<ScanJob>>;

    /// Stop `library_id`'s scan before the library is deleted.
    ///
    /// The library is retired first: while the returned
    /// [`StoppedScan::retirement`] is held no scan registers for it and no
    /// watcher event reconciles it, so nothing starts between this call and
    /// the caller's delete. The caller commits the retirement once the delete
    /// succeeds; dropping it -- the delete failed -- gives the library back.
    /// A queued scan fails as [`CANCELLED`] at once; a running one is asked
    /// to stop after the file it is on and waited for -- as [`CANCELLED`],
    /// unless it finished first -- for at most [`SCAN_STOP_TIMEOUT`] on the
    /// injected clock. [`StoppedScan::in_time`] says whether no scan of the
    /// library is still queued or running.
    async fn stop_scan(&self, library_id: Uuid) -> StoppedScan;

    /// Drop `library_id`'s lock and latest job once the library is deleted.
    fn forget_library(&self, library_id: Uuid);
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
    /// Asked when each candidate row was last played, to break a tie
    /// between identical copies of a moved file (issue #180).
    progress_repo: Arc<dyn WatchStateRepository>,
    path_policy: PathPolicy,
    enrichment_repo: Option<Arc<dyn EnrichmentStateRepository>>,
    sidecar_repo: Option<Arc<dyn SidecarSubtitleRepository>>,
    /// Where each probed file's chapters go (issue #265). Unset, none are
    /// recorded.
    chapter_repo: Option<Arc<dyn ChapterRepository>>,
    applied_nfo_repo: Option<Arc<dyn AppliedNfoRepository>>,
    /// Users' watchlists and favourites (issue #262), whose entries a title
    /// merge carries onto the survivor. Unset, a merge carries none.
    user_list_repo: Option<Arc<dyn UserListRepository>>,
    divergence_policy: DivergencePolicy,
    clock: Arc<dyn Clock>,
    id_generator: Arc<dyn IdGenerator>,
    missing_file_grace: Duration,
    /// How long a file must go unwritten before it is hashed (issue #181).
    settle_window: Duration,
    /// Which libraries' inode numbers are compared ([`Inodes`]).
    filesystem_probe: Arc<dyn FilesystemProbe>,
    /// Whether [`LocalIndexService::backfill_identity_keys`] and then
    /// [`LocalIndexService::rekey_stale_titles`] have both succeeded in this
    /// process; until they have, no file classified by older rules is
    /// reclassified. The passes run only while holding the coordinator's
    /// catalog gate exclusively, so two callers never run them at once, and
    /// no scan or reconcile classifies a file while they move keys.
    identity_passes_succeeded: AtomicBool,
    /// Serialises every scan and reconcile of a library, and gates them
    /// against the identity passes.
    scans: ScanCoordinator,
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
        progress_repo: Arc<dyn WatchStateRepository>,
    ) -> Self {
        Self {
            progress_repo,
            library_repo,
            file_repo,
            movie_repo,
            show_repo,
            stream_repo,
            hash_service,
            media_info_service,
            notification_service,
            admin_log,
            path_policy: PathPolicy::default(),
            enrichment_repo: None,
            sidecar_repo: None,
            chapter_repo: None,
            applied_nfo_repo: None,
            user_list_repo: None,
            divergence_policy: DivergencePolicy::default(),
            clock: Arc::new(RealClock),
            id_generator: Arc::new(UuidGenerator),
            missing_file_grace: DEFAULT_MISSING_FILE_GRACE,
            settle_window: Duration::ZERO,
            filesystem_probe: Arc::new(StatfsFilesystemProbe),
            identity_passes_succeeded: AtomicBool::new(false),
            scans: ScanCoordinator::new(),
        }
    }

    /// Override the clock that stamps scan times and `missing_since`, and that
    /// the grace period is measured against. Defaults to [`RealClock`].
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Override where scan-job ids come from. Defaults to [`UuidGenerator`].
    pub fn with_id_generator(mut self, id_generator: Arc<dyn IdGenerator>) -> Self {
        self.id_generator = id_generator;
        self
    }

    /// Hash a file only once it has gone `window` without a write, measured
    /// from its modification time against the injected clock (issue #181):
    /// a file still being copied in is left alone -- a scan counts it as
    /// deferred, a watcher event retries it once the window has passed --
    /// rather than hashed and probed while partial. A file that changes
    /// while it is hashed is deferred the same way. Zero, the default, hashes
    /// every file on sight.
    pub fn with_settle_window(mut self, window: Duration) -> Self {
        self.settle_window = window;
        self
    }

    /// Override how a library root's filesystem is classified, which decides
    /// whether its files' inode numbers are compared ([`Inodes`]). Defaults
    /// to [`StatfsFilesystemProbe`], the watcher's own classification.
    pub fn with_filesystem_probe(mut self, probe: Arc<dyn FilesystemProbe>) -> Self {
        self.filesystem_probe = probe;
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

    /// Override which paths under a library root are indexed -- to add an
    /// administrator's ignore patterns (issue #182). Defaults to
    /// [`PathPolicy::default`]: video files, less hidden files, housekeeping
    /// folders and extras.
    pub fn with_path_policy(mut self, policy: PathPolicy) -> Self {
        self.path_policy = policy;
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

    /// Wire up sidecar subtitles (issue #184): when set, every text subtitle
    /// beside an indexed video is recorded as a subtitle of that video.
    /// Defaults to `None`, which indexes no sidecar subtitles.
    pub fn with_sidecar_repo(mut self, repo: Arc<dyn SidecarSubtitleRepository>) -> Self {
        self.sidecar_repo = Some(repo);
        self
    }

    /// Wire up chapters (issue #265): when set, every probe of a file
    /// replaces its chapters with the ones its container lists, and a
    /// changed file that no longer probes is left with none. Defaults to
    /// `None`, which records no chapters.
    pub fn with_chapter_repo(mut self, repo: Arc<dyn ChapterRepository>) -> Self {
        self.chapter_repo = Some(repo);
        self
    }

    /// Replace the chapters of the file `file_id` with `chapters`, when
    /// chapters are wired.
    async fn replace_chapters(&self, file_id: Uuid, chapters: Vec<Chapter>) -> Result<(), DbErr> {
        match &self.chapter_repo {
            Some(repo) => repo.replace_for_file(file_id, chapters).await,
            None => Ok(()),
        }
    }

    /// Wire up users' watchlists and favourites (issue #262): when set, a
    /// title merge carries the retired title's entries onto the survivor
    /// rather than letting them go with it.
    pub fn with_user_list_repo(mut self, repo: Arc<dyn UserListRepository>) -> Self {
        self.user_list_repo = Some(repo);
        self
    }

    /// Carry users' list entries for `from` onto `to`, when lists are wired.
    async fn carry_list_entries(&self, from: TitleRef, to: TitleRef) -> Result<(), DbErr> {
        match &self.user_list_repo {
            Some(lists) => lists.carry(from, to).await,
            None => Ok(()),
        }
    }

    /// Wire up what each NFO held when it was last applied (issue #184): when
    /// set, a scan re-applies exactly the NFOs whose content changed since,
    /// and an NFO a watcher event reports unchanged is not applied again.
    /// Defaults to `None`, which leaves re-applying NFOs to watcher events
    /// alone: without a record, a scan cannot tell an edited NFO from one it
    /// already applied.
    pub fn with_applied_nfo_repo(mut self, repo: Arc<dyn AppliedNfoRepository>) -> Self {
        self.applied_nfo_repo = Some(repo);
        self
    }

    /// The repository backing this service's library lookups, exposed so
    /// callers (e.g. background maintenance tasks) can list/query libraries
    /// without needing their own separate handle to the same repository.
    pub fn library_repo(&self) -> &Arc<dyn LibraryRepository> {
        &self.library_repo
    }

    /// Helper to extract and insert media streams for a file, and to replace
    /// its chapters with the ones the same probe read
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
                        is_hearing_impaired: s.disposition.is_hearing_impaired(),
                    });
                    (metadata, StreamType::Subtitle)
                }
            };

            streams_to_insert.push(CreateMediaStream {
                file_id,
                index: stream.index() as u32,
                stream_type,
                // FFmpeg's own codec name, the one vocabulary every stream
                // kind and every sidecar subtitle is recorded in (#189).
                codec: match stream {
                    StreamMetadata::Video(v) => v.video.codec_name.clone(),
                    StreamMetadata::Audio(a) => a.audio.codec_name.clone(),
                    StreamMetadata::Subtitle(s) => s.codec_name.clone(),
                },
                metadata: stream_metadata,
            });
        }

        let count = self.stream_repo.insert_streams(streams_to_insert).await?;
        // The chapters the same probe read, in the container's order.
        let chapters = metadata
            .chapters
            .iter()
            .enumerate()
            .map(|(index, chapter)| Chapter {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
                title: chapter.title.clone(),
            })
            .collect();
        self.replace_chapters(file_id, chapters).await?;
        Ok(count)
    }

    /// Classify a file from its path relative to `library.root_path`, the
    /// NFOs beside it and its container `tags` (issue #184), finding or
    /// creating its movie or show: by the provider id an NFO pins it to, else
    /// by identity key (issues #182, #183). The key is always the path's; an
    /// NFO or a tag only says what a new title is shown as.
    ///
    /// `None` when the path says the file is media but not which: an episode
    /// file with no episode number in a season folder. Such a file is kept as
    /// an `Unknown` row with no content, and the administrator is told.
    async fn classify_media_content(
        &self,
        path: &Path,
        library: &Library,
        runtime: Option<Duration>,
        tags: &ContainerTags,
    ) -> Result<Option<MediaFileContent>, IndexError> {
        let hints::NfoFiles {
            file: file_nfo,
            show: show_nfo,
        } = hints::locate_nfos(&library.root_path, path);
        let Classification {
            inference,
            display,
            pin,
        } = classify(
            relative_to(&library.root_path, path),
            &Hints {
                file_nfo: file_nfo.as_ref().map(|located| &located.nfo),
                show_nfo: show_nfo.as_ref().map(|located| &located.nfo),
                tags,
            },
        );
        // What classification read is applied by it (FR-219): a movie by its
        // own NFO, an episode by its show's and its own.
        let consumed: Vec<&hints::LocatedNfo> = match &inference {
            MediaInference::Movie(_) => file_nfo.iter().collect(),
            MediaInference::Episode(_) => show_nfo.iter().chain(file_nfo.iter()).collect(),
            MediaInference::Unclassifiable(_) => Vec::new(),
        };
        match inference {
            MediaInference::Episode(EpisodeInference {
                series,
                season: season_num,
                first_episode,
                last_episode,
                air_date,
                episode_title,
                numbering: _,
                contradicted_season_folder,
            }) => {
                if let Some(folder) = contradicted_season_folder {
                    warn!(
                        path = %path.display(),
                        folder_season = folder,
                        file_season = season_num,
                        "the filename names a different season than its season folder; \
                         the filename wins"
                    );
                }
                // One `ON CONFLICT` statement on the show's identity key: the
                // show is found however enrichment has since renamed it, and
                // two episodes of a new show indexed at once share one row.
                // A new show is shown as its NFO or tags name it.
                let mut create = CreateShow::new(series.title, series.year);
                if let Some(TitleGuess { title, year }) = display {
                    create.title = title;
                    create.year = year;
                }
                let show = self.show_for(create, pin.as_ref(), path).await?;
                self.record_consumed_nfos(library, &consumed).await?;

                // Ensure library-show association exists
                self.show_repo
                    .ensure_library_association(library.id, show.id)
                    .await?;

                let season = self
                    .show_repo
                    .find_or_create_season(show.id, season_num)
                    .await?;

                // Find or create the episode. A second file for the same
                // (season, episode) -- another resolution, another encode --
                // attaches to the existing episode as another source rather
                // than colliding with it; the episode's title and runtime stay
                // those the first file (or enrichment since) established. A
                // multi-episode file attaches to its first episode and carries
                // the rest of its range itself. Its runtime is the whole
                // range's, so it is not the episode's: a new episode from it
                // has none until enrichment supplies one (#189).
                let spans_episodes = last_episode.is_some_and(|last| last > first_episode);
                let create_episode = CreateEpisode {
                    season_id: season.id,
                    episode_number: first_episode,
                    title: episode_title.unwrap_or_else(|| format!("Episode {first_episode}")),
                    runtime: if spans_episodes { None } else { runtime },
                    air_date,
                };
                let episode = self
                    .show_repo
                    .find_or_create_episode(create_episode)
                    .await?;

                Ok(Some(MediaFileContent::Episode {
                    episode_id: episode.id,
                    last_episode_number: last_episode,
                }))
            }
            MediaInference::Movie(MovieInference {
                title,
                edition,
                part_number,
            }) => {
                // A disc's stream file is the part of the film its place in
                // the disc's main title makes it, which only the disc says
                // (issue #234).
                let part_number = if disc_stream_kind(path).is_some() {
                    self.disc_part(library, path)
                } else {
                    part_number
                };
                // Found by pin or identity key, never by display title:
                // enrichment may have renamed the movie since its first file
                // (#183). A new movie is shown as its NFO names it. One part
                // of a multi-part movie lasts that part, not the movie: a new
                // movie from it has no runtime until enrichment supplies one,
                // as a new episode from a multi-episode file has none.
                let runtime = if part_number.is_some() { None } else { runtime };
                let mut create = CreateMovie::new(title.title, title.year, runtime);
                if let Some(TitleGuess { title, year }) = display {
                    create.title = title;
                    create.year = year;
                }
                let movie = self.movie_for(create, pin.as_ref(), path).await?;
                self.record_consumed_nfos(library, &consumed).await?;

                // Ensure library-movie association exists
                self.movie_repo
                    .ensure_library_association(library.id, movie.id)
                    .await?;

                // One entry per edition of the film in this library: every
                // copy of the same edition -- and every part of one -- is
                // another file of that entry.
                let entry = self
                    .movie_repo
                    .find_or_create_entry(CreateMovieEntry {
                        library_id: library.id,
                        movie_id: movie.id,
                        edition,
                    })
                    .await?;

                Ok(Some(MediaFileContent::Movie {
                    movie_entry_id: entry.id,
                    part_number,
                }))
            }
            MediaInference::Unclassifiable(reason) => {
                self.report_unclassifiable(library, path, reason).await;
                Ok(None)
            }
        }
    }

    /// The part the disc stream file at `path` is of the source its disc is
    /// part of, read from the disc -- and from the other discs of its set,
    /// if it is one of a set ([`disc::read_source`]) -- by [`disc::part_in`]:
    /// `None` for a source of one file, for a file the source does not play,
    /// or when the source could not be read whole.
    fn disc_part(&self, library: &Library, path: &Path) -> Option<u32> {
        let root = library.root_path.as_path();
        let (disc_root, kind) = self.path_policy.disc_root(relative_to(root, path))?;
        let read = disc::read_source(root, &root.join(disc_root), kind, &self.path_policy);
        disc::part_in(&read.files, path)
    }

    /// Give each file of the sources the disc structures rooted at `discs`
    /// are part of the part its place in its source makes it, where its row
    /// holds another (issue #234). A row keeps its part until something
    /// reads its disc again, and a disc's main title can change under rows
    /// that are otherwise unchanged -- a missing VOB restored, a one-file
    /// title grown to two, a disc of a set arriving or leaving -- so every
    /// walk that reads a disc re-derives the parts of its whole source. A
    /// source that could not be read whole changes no part.
    ///
    /// A disc of a set is read with every other disc of its set
    /// ([`disc::set_members`]), whether or not they are one source: a disc
    /// read may have made its set whole or broken it -- arrived, emptied in
    /// place, or claimed a number another disc has -- and every other disc
    /// of the set is then a source of its own, or part of the one run, where
    /// it was not before.
    async fn rederive_disc_parts(&self, library: &Library, discs: &[PathBuf]) -> Result<(), DbErr> {
        let root = library.root_path.as_path();
        let mut discs_and_sets: Vec<PathBuf> = Vec::with_capacity(discs.len());
        let mut sets_read: std::collections::HashSet<DiscSetPlace> =
            std::collections::HashSet::new();
        for disc_root in discs {
            discs_and_sets.push(disc_root.clone());
            let Some((rel, _)) = self.path_policy.disc_root(relative_to(root, disc_root)) else {
                continue;
            };
            let Some(place) = disc_set_place(&rel) else {
                continue;
            };
            if !sets_read.insert(place.clone()) {
                continue;
            }
            // A set whose folder cannot be listed is read by `read_source`
            // below as a source that could not be read, which changes no
            // part.
            if let Ok(members) = disc::set_members(root, &place, &self.path_policy) {
                discs_and_sets.extend(members.into_iter().map(|(_, member, _)| member));
            }
        }
        let mut done: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut discs_read: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        for disc_root in &discs_and_sets {
            if !discs_read.insert(disc_root.as_path()) {
                continue;
            }
            let Some((rel, kind)) = self.path_policy.disc_root(relative_to(root, disc_root)) else {
                continue;
            };
            let disc::SourceRead { files, failed } =
                disc::read_source(root, &root.join(rel), kind, &self.path_policy);
            if failed {
                continue;
            }
            for path in &files {
                if !done.insert(path.clone()) {
                    continue;
                }
                let Some(file) = self.file_repo.find_by_path(&path.to_string_lossy()).await? else {
                    continue;
                };
                let Some(MediaFileContent::Movie {
                    movie_entry_id,
                    part_number,
                }) = file.content
                else {
                    continue;
                };
                let part = disc::part_in(&files, path);
                if part == part_number {
                    continue;
                }
                debug!(path = %path.display(), from = ?part_number, to = ?part, "a disc file's place in its source changed");
                self.move_file(
                    &file,
                    MediaFileContent::Movie {
                        movie_entry_id,
                        part_number: part,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Re-derive the parts of the set of discs that `path` -- a path just
    /// gone -- was a disc of, or a disc folder of ([`Self::rederive_disc_parts`]):
    /// the discs left in the set may now be a whole run, or no longer one.
    async fn rederive_disc_set_of(&self, library: &Library, path: &Path) -> Result<(), DbErr> {
        let root = library.root_path.as_path();
        let rel = relative_to(root, path);
        let place = match self.path_policy.disc_root(rel) {
            Some((disc_root, _)) => disc_set_place(&disc_root),
            None => disc_set_member(rel),
        };
        let Some(place) = place else {
            return Ok(());
        };
        let Ok(members) = disc::set_members(root, &place, &self.path_policy) else {
            return Ok(());
        };
        let discs: Vec<PathBuf> = members.into_iter().map(|(_, disc, _)| disc).collect();
        self.rederive_disc_parts(library, &discs).await
    }

    /// Tell the administrator a file was indexed without a title, and why.
    async fn report_unclassifiable(
        &self,
        library: &Library,
        path: &Path,
        reason: UnclassifiableReason,
    ) {
        let (message, details) = match reason {
            UnclassifiableReason::NoEpisodeNumberInSeasonFolder { season } => {
                warn!(
                    path = %path.display(),
                    season,
                    "a file in a season folder has no episode number; indexed without a title"
                );
                (
                    format!(
                        "A file in a season folder of \"{}\" has no episode number, so it was \
                         indexed without a title: {}",
                        library.name,
                        path.display()
                    ),
                    serde_json::json!({ "season_folder": season }),
                )
            }
            UnclassifiableReason::AmbiguousAbsoluteNumber { number } => {
                warn!(
                    path = %path.display(),
                    number,
                    "a file is numbered like an episode but no folder names its show; indexed \
                     without a title"
                );
                (
                    format!(
                        "A file in \"{}\" is numbered like episode {number} of a show, but no \
                         folder names the show, so it was indexed without a title: {}",
                        library.name,
                        path.display()
                    ),
                    serde_json::json!({ "absolute_number": number }),
                )
            }
            UnclassifiableReason::FractionalAbsoluteNumber { whole, tenth } => {
                warn!(
                    path = %path.display(),
                    number = %format!("{whole}.{tenth}"),
                    "a file is numbered like a recap between two episodes; indexed without a title"
                );
                (
                    format!(
                        "A file in \"{}\" is numbered {whole}.{tenth}, like a recap or special \
                         between two episodes, which has no episode number of its own, so it \
                         was indexed without a title: {}",
                        library.name,
                        path.display()
                    ),
                    serde_json::json!({ "fractional_number": format!("{whole}.{tenth}") }),
                )
            }
            UnclassifiableReason::DiscWithoutTitleFolder => {
                warn!(
                    path = %path.display(),
                    "a disc structure is in no folder naming its film; indexed without a title"
                );
                (
                    format!(
                        "A DVD or Blu-ray folder in \"{}\" is not inside a folder naming its \
                         film (it is at the library root, or in a season folder), so it was \
                         indexed without a title: {}",
                        library.name,
                        path.display()
                    ),
                    serde_json::json!({ "disc_without_title_folder": true }),
                )
            }
            UnclassifiableReason::NoEpisodeMarkerInMultiSeasonFolder => {
                warn!(
                    path = %path.display(),
                    "a file in a multi-season folder has no season and episode marker; indexed \
                     without a title"
                );
                (
                    format!(
                        "A file in a multi-season folder of \"{}\" has no season and episode \
                         marker, so it was indexed without a title: {}",
                        library.name,
                        path.display()
                    ),
                    serde_json::json!({ "multi_season_folder": true }),
                )
            }
        };
        let mut metadata = serde_json::json!({
            "library_id": library.id.to_string(),
            "path": path.display().to_string(),
        });
        if let (Some(metadata), serde_json::Value::Object(details)) =
            (metadata.as_object_mut(), details)
        {
            metadata.extend(details);
        }
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::LibraryScan,
                message,
                Some(metadata),
            )
            .await;
    }

    /// Bring a row classified by older rules up to [`CLASSIFIER_VERSION`]:
    /// reclassify it from its path, the NFOs beside it and the container tags
    /// its last probe stored, attaching it to the title the current rules
    /// name, and stamp the version. The row keeps its id -- and so stays the
    /// file a viewer's watch state last played -- its hash and its probe
    /// results; nothing is re-probed. A title the move leaves with no file (a
    /// show a legacy build named after a `Season 01` folder) is retired by
    /// the scan's orphan cleanup, its viewers' watch state carried first to
    /// the title the file joins.
    ///
    /// Only a row that was probed is reclassified: one whose probe failed has
    /// no runtime and was never classified.
    async fn reclassify_existing(
        &self,
        existing: &MediaFile,
        path: &Path,
        library: &Library,
    ) -> Result<(), IndexError> {
        // The tags the file's last probe read, as stored: a file placed by
        // its tags keeps its place without being probed again.
        let tags = existing.container_tags.clone().unwrap_or_default();
        let content = self
            .classify_media_content(path, library, existing.duration, &tags)
            .await?;
        let status = match (&content, existing.status) {
            (None, _) => FileStatus::Unknown,
            (Some(_), FileStatus::Unknown) => FileStatus::Known,
            (Some(_), status) => status,
        };
        // A title the move empties goes with orphan cleanup, and its viewers'
        // state (issue #246) and users' list entries (issue #262) with it
        // unless carried first.
        if let (Some(from), Some(to)) = (&existing.content, &content) {
            self.carry_from_emptied_title(existing.id, from, to).await?;
        }
        self.file_repo
            .set_classification(
                existing.id,
                FileClassification {
                    content,
                    status,
                    classifier_version: CLASSIFIER_VERSION,
                },
            )
            .await?;
        Ok(())
    }

    /// Process a NEW media file to add it to the library. The caller has
    /// already established that the [`PathPolicy`] calls `path` media.
    ///
    /// Stat, settle, hash, relink, probe, classify, write -- in that order. A
    /// file still being written is left alone and nothing is written for it
    /// ([`FileOutcome::Deferred`]). Every file is hashed before it is probed,
    /// so a row whose probe fails still carries its real hash and takes part
    /// in duplicate detection like any other.
    ///
    /// A file that is a moved or renamed one -- see
    /// [`choose_relink_candidate`], over the rows `relink` offers -- is
    /// [relinked](Self::relink_file) instead: its old row, with its id and
    /// everything keyed on it, points at the new path, and the file is
    /// neither probed nor classified (issue #180).
    ///
    /// `known` is what a scan found at the path before, reused when the file
    /// is still as it was found ([`FileStat::is_same_as`]) rather than hashed
    /// again. `inodes` is what the library's filesystem keeps.
    ///
    /// The file is opened once, beneath the library root with no link
    /// followed ([`LibraryFile::open`]), and its stat, hash and probe are all
    /// read from that handle (issue #238): a file or a folder above it
    /// swapped for a link since the walk fails to open, and nothing is
    /// recorded for it.
    async fn process_new_file(
        &self,
        path: &Path,
        library: &Library,
        relink: RelinkSource,
        known: Option<&Fingerprint>,
        inodes: Inodes,
    ) -> Result<FileOutcome, IndexError> {
        info!("Processing new file: {}", path.display());

        let root = library.root_path.as_path();
        let opened = LibraryFile::open(root, path).map_err(|e| {
            warn!(path = %path.display(), error = %e, "Failed to read file metadata");
            IndexError::PathNotFound(format!("Could not read file metadata: {e}"))
        })?;
        let stat = FileStat::of(&FileMeta::from(opened.metadata()), inodes);
        let FileStat {
            size,
            mtime,
            identity,
            inodes: _,
        } = stat;

        if let Settle::Unsettled { retry_after } =
            settle_state(self.clock.now(), mtime, self.settle_window)
        {
            debug!(path = %path.display(), "a new file is still being written; deferring it");
            return Ok(FileOutcome::Deferred(retry_after));
        }

        let hash = match known {
            Some(found) if stat.is_same_as(&found.stat()) => Some(found.hash),
            _ => self.hash_settled(root, &opened, stat).await.map_err(|e| {
                error!(path = %path.display(), error = %e, "Failed to hash file");
                IndexError::PathNotFound(format!("Hash failed: {}", e))
            })?,
        };
        let Some(hash) = hash else {
            debug!(path = %path.display(), "a new file changed while it was hashed; deferring it");
            return Ok(FileOutcome::Deferred(self.settle_window));
        };

        if let RelinkSource::Repository = relink {
            let candidates: Vec<MediaFile> = self
                .file_repo
                .find_by_library_and_hash_including_missing(library.id, hash)
                .await?
                .into_iter()
                .filter(|row| row.size_bytes == size && row.path != path)
                .collect();
            let last_played = self.last_played(&candidates).await?;
            let found = Fingerprint {
                size,
                mtime,
                identity,
                inodes,
                hash,
            };
            if let Some(row) = choose_relink_candidate(
                path,
                size,
                hash,
                &candidates,
                |old| path_is_absent(root, old),
                &last_played,
            ) {
                self.relink_files(&[(row.clone(), path.to_path_buf(), found)], &[], library)
                    .await?;
                return Ok(FileOutcome::Relinked);
            }
            // A row whose path now holds something else may be this file,
            // moved in a swap or a rotation whose other halves have not
            // reached the watcher: the scan sees them all.
            if hash != 0
                && candidates
                    .iter()
                    .any(|row| may_have_moved(root, row, inodes))
            {
                info!(
                    path = %path.display(),
                    "a new file matches a file of the library that may have moved; leaving it to the next scan"
                );
                return Ok(FileOutcome::LeftToScan);
            }
        }

        let metadata = match self.media_info_service.get_video_metadata(opened).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to extract metadata for {}: {}", path.display(), e);
                // Never classified: version 0 until a probe succeeds, which
                // a later visit retries (see `reconcile_existing_file`).
                let file = self
                    .file_repo
                    .create(CreateMediaFile {
                        library_id: library.id,
                        path: path.to_path_buf(),
                        hash,
                        size_bytes: size,
                        mtime,
                        identity,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: None,
                        status: FileStatus::Unknown,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await?;
                self.check_and_report_duplicate(&file).await;
                return Ok(FileOutcome::Added);
            }
        };

        let duration = Duration::from_secs_f64(metadata.duration_seconds());
        let tags = container_tags(&metadata);
        let content = self
            .classify_media_content(path, library, Some(duration), &tags)
            .await?;
        let status = if content.is_some() {
            FileStatus::Known
        } else {
            FileStatus::Unknown
        };

        let file = self
            .file_repo
            .create(CreateMediaFile {
                library_id: library.id,
                path: path.to_path_buf(),
                hash,
                size_bytes: size,
                mtime,
                identity,
                mime_type: Some(container_mime_type(&metadata.format_name, path).to_owned()),
                duration: Some(duration),
                container_format: Some(metadata.format_name.clone()),
                content,
                status,
                classifier_version: CLASSIFIER_VERSION,
                container_tags: Some(tags),
            })
            .await?;

        self.insert_media_streams(file.id, &metadata).await?;
        self.check_and_report_duplicate(&file).await;
        self.check_and_report_runtime_divergence(&file).await;
        Ok(FileOutcome::Added)
    }

    /// Hash `opened`, a file beneath the library `root` whose handle a stat
    /// just before found as `stat`, from that handle.
    ///
    /// With a settle window, its path is stat'ed again afterwards: `None`
    /// when the file changed while it was read, since the hash then
    /// describes no version of the file that ever existed whole.
    async fn hash_settled(
        &self,
        root: &Path,
        opened: &LibraryFile,
        stat: FileStat,
    ) -> std::io::Result<Option<u64>> {
        let hash = self
            .hash_service
            .hash_async(opened.try_clone_file()?)
            .await?;
        if self.settle_window.is_zero() {
            return Ok(Some(hash));
        }
        let after = read_stat(root, opened.path(), stat.inodes)?;
        if !after.is_same_as(&stat) {
            return Ok(None);
        }
        Ok(Some(hash))
    }

    /// Whether `library`'s filesystem keeps its files' inode numbers
    /// ([`Inodes`]), asked once per scan or watcher event. A root that cannot
    /// be classified is taken as local, as the watcher takes it: its inodes
    /// are compared, which at worst hashes a file that did not change.
    fn inodes_of(&self, library: &Library) -> Inodes {
        match self.filesystem_probe.kind(&library.root_path) {
            Ok(kind) => Inodes::from(kind),
            Err(e) => {
                warn!(
                    library_id = %library.id,
                    error = %e,
                    "could not classify a library's filesystem; comparing its inode numbers"
                );
                Inodes::Stable
            }
        }
    }

    /// Clear `missing_since` on a row whose path is back on disk, returning
    /// whether it had been missing. The row keeps its id, so a viewer's watch
    /// state that last played it resumes on it again (issue #179). Shared by
    /// the full scan and single-path watcher events.
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

    /// When each of `rows` was last played; asked only when there are two
    /// rows to weigh against each other -- a tie to break, or a path's row
    /// against the row a relink would give the path.
    async fn last_played(
        &self,
        rows: &[MediaFile],
    ) -> Result<HashMap<Uuid, DateTime<Utc>>, IndexError> {
        if rows.len() < 2 {
            return Ok(HashMap::new());
        }
        Ok(self
            .progress_repo
            .last_played_at(rows.iter().map(|row| row.id).collect())
            .await?)
    }

    /// Reconcile a file already present in the index against its current state
    /// on disk. Shared by the full scan and single-path watcher events.
    ///
    /// A row classified by older rules is reclassified first, whether or not
    /// the file changed: the rules changed, not the file. Only when
    /// `reclassify` is set -- the identity passes have succeeded (see
    /// [`Self::identity_passes_done`]); until then the row keeps its title
    /// and its version, and a later scan reclassifies it.
    ///
    /// A video file whose probe has never succeeded (a container whose index
    /// was not written yet, a file probed mid-copy) is probed again on every
    /// visit, changed or not, and classified the first time a probe
    /// succeeds. It is rehashed only if its row no longer records it as it is
    /// -- its size, modification time or identity moved
    /// ([`FileStat::is_recorded_by`]) -- or it was never hashed. A row with
    /// no identity yet (one recorded before identities were) is given the
    /// file's when it is found unchanged, without a hash.
    ///
    /// A changed file is hashed only once it has settled; until then it is
    /// [`FileOutcome::Deferred`] and its row is left as it is. `known` is
    /// what a scan found at the path before, reused when the file is still
    /// as it was found ([`FileStat::is_same_as`]) rather than hashed again.
    /// `stats` stats files beneath the library's root, as a row records them.
    ///
    /// The file is stat'ed beneath the library root with no link followed,
    /// and opened -- the same way -- only when it is to be hashed or probed;
    /// its stat, hash and probe are then all read from that one handle (issue
    /// #238). A file that is no longer a regular file reached without a link
    /// has its row marked missing; one that fails to stat or open for any
    /// other reason is left as it is.
    async fn reconcile_existing_file(
        &self,
        existing: &MediaFile,
        path: &Path,
        library: &Library,
        reclassify: bool,
        known: Option<&Fingerprint>,
        stats: &mut LibraryStats,
    ) -> Result<FileOutcome, IndexError> {
        if reclassify && awaits_reclassification(existing) {
            self.reclassify_existing(existing, path, library).await?;
        }

        let root = library.root_path.as_path();
        let listed = match stats.stat(path) {
            Ok(listed) => listed,
            Err(e) => return self.unreadable_existing_file(existing, path, &e).await,
        };
        // Only a file the path policy calls media -- a video file -- is
        // reconciled at all, so nothing else is ever probed here.
        let unprobed = existing.duration.is_none();

        // Cheap gate: only a size, mtime or identity change warrants a
        // rehash, and only an unprobed file a re-probe. Neither opens the
        // file.
        if listed.is_recorded_by(existing) && !unprobed {
            let FileStat {
                size: _,
                mtime: _,
                identity,
                inodes: _,
            } = listed;
            if existing.identity.is_none() && identity.is_some() {
                // Recorded before identities were: its size and mtime are
                // all there is to go on, and they match. Record the identity
                // it has now, so the next scan can tell a swap from it,
                // rather than hash every such file at once (issue #228).
                self.file_repo
                    .update(UpdateMediaFile {
                        id: existing.id,
                        hash: None,
                        size_bytes: None,
                        mtime: None,
                        identity,
                        probe: ProbeUpdate::Keep,
                        content: None,
                        status: None,
                    })
                    .await?;
            }
            record_file_outcome("unchanged");
            return Ok(FileOutcome::Unchanged);
        }

        let opened = match LibraryFile::open(root, path) {
            Ok(opened) => opened,
            Err(e) => return self.unreadable_existing_file(existing, path, &e).await,
        };
        let stat = FileStat::of(&FileMeta::from(opened.metadata()), stats.inodes());
        let FileStat {
            size,
            mtime,
            identity,
            inodes: _,
        } = stat;
        let moved = !stat.is_recorded_by(existing);

        if moved
            && let Settle::Unsettled { retry_after } =
                settle_state(self.clock.now(), mtime, self.settle_window)
        {
            debug!(path = %path.display(), "a changed file is still being written; deferring it");
            return Ok(FileOutcome::Deferred(retry_after));
        }

        let known = known
            .filter(|found| stat.is_same_as(&found.stat()))
            .map(|found| found.hash);
        let new_hash = if let Some(hash) = known {
            hash
        } else if moved || existing.hash == 0 {
            // Rehash to confirm the content actually changed.
            match self.hash_settled(root, &opened, stat).await {
                Ok(Some(h)) => h,
                Ok(None) => {
                    debug!(path = %path.display(), "a file changed while it was hashed; deferring it");
                    return Ok(FileOutcome::Deferred(self.settle_window));
                }
                Err(e) => {
                    warn!("Failed to hash {}: {}", path.display(), e);
                    record_file_outcome("failed");
                    return Ok(FileOutcome::Failed);
                }
            }
        } else {
            existing.hash
        };

        if new_hash == existing.hash && !unprobed {
            // Content unchanged (e.g. mtime bumped by `touch`, or a file
            // rewritten with the same bytes): refresh size, mtime and
            // identity.
            self.file_repo
                .update(UpdateMediaFile {
                    id: existing.id,
                    hash: None,
                    size_bytes: Some(size),
                    mtime,
                    identity,
                    probe: ProbeUpdate::Keep,
                    content: None,
                    status: None,
                })
                .await?;
            record_file_outcome("unchanged");
            return Ok(FileOutcome::Unchanged);
        }

        let changed = self
            .reprobe_file(existing, opened, library, stat, new_hash)
            .await?;
        if changed {
            record_file_outcome("changed");
            Ok(FileOutcome::Changed)
        } else {
            record_file_outcome("unchanged");
            Ok(FileOutcome::Unchanged)
        }
    }

    /// What becomes of `existing` when its file at `path` failed to stat or
    /// open with `err`. A file that is no longer a regular file reached
    /// without a link ([`is_refusal`]) is no file of the library, so its row
    /// is marked missing, as a scan's walk would leave it (issue #238). Any
    /// other failure says nothing about the file -- it is transient, or a
    /// permission error -- and must not delete or corrupt the row (issue
    /// #179).
    async fn unreadable_existing_file(
        &self,
        existing: &MediaFile,
        path: &Path,
        err: &std::io::Error,
    ) -> Result<FileOutcome, IndexError> {
        if !is_refusal(err) {
            warn!("Failed to stat {}: {}", path.display(), err);
            return Ok(FileOutcome::Unchanged);
        }
        // Marked whatever `existing` says: the row may have been restored
        // since it was read. A row already missing keeps its first stamp.
        let marked = self
            .file_repo
            .mark_missing(vec![existing.id], self.clock.now())
            .await?;
        if marked > 0 {
            info!(
                path = %path.display(),
                error = %err,
                "an indexed file is no longer a regular file reached without a link; marked it missing"
            );
        }
        Ok(FileOutcome::Unchanged)
    }

    /// Probe `existing` again -- its content changed to `new_hash`, or its
    /// last probe failed -- and bring its row in line, returning whether the
    /// row changed.
    ///
    /// A successful probe replaces the file's metadata and streams. A row
    /// with no movie or episode (one whose first probe failed, or whose path
    /// named no title) is then classified from its path, as a new file would
    /// be; one with a title keeps it, since the path -- and so the inferred
    /// title -- has not moved.
    ///
    /// A failed probe of a file whose content changed keeps its title and is
    /// marked `Changed`; one with no title stays `Unknown`, which is all the
    /// `files` CHECK allows it to be. Either way its streams and probe
    /// results are cleared ([`ProbeUpdate::Clear`]): they described the old
    /// content, and a row with no duration is probed again on every visit. A
    /// failed probe of a file whose content did not change writes only the
    /// size, modification time and identity it was found with, so the next
    /// visit -- which tries the probe again -- does not rehash it.
    ///
    /// The probe reads `opened`, the handle the file was stat'ed and hashed
    /// from (issue #238).
    async fn reprobe_file(
        &self,
        existing: &MediaFile,
        opened: LibraryFile,
        library: &Library,
        stat: FileStat,
        new_hash: u64,
    ) -> Result<bool, IndexError> {
        let path = opened.path().to_path_buf();
        let path = path.as_path();
        let FileStat {
            size,
            mtime,
            identity,
            inodes: _,
        } = stat;
        let content_changed = new_hash != existing.hash;
        if content_changed {
            info!("File content changed, reconciling: {}", path.display());
        }

        match self.media_info_service.get_video_metadata(opened).await {
            Ok(metadata) => {
                // Replace the file's stream set with the freshly extracted one.
                self.stream_repo.delete_by_file_id(existing.id).await?;
                self.insert_media_streams(existing.id, &metadata).await?;

                let duration = Duration::from_secs_f64(metadata.duration_seconds());
                let tags = container_tags(&metadata);
                let mut updated = self
                    .file_repo
                    .update(UpdateMediaFile {
                        id: existing.id,
                        hash: Some(new_hash),
                        size_bytes: Some(size),
                        mtime,
                        identity,
                        probe: ProbeUpdate::Set {
                            mime_type: container_mime_type(&metadata.format_name, path).to_owned(),
                            duration,
                            container_format: metadata.format_name.clone(),
                            container_tags: tags.clone(),
                        },
                        content: None,
                        status: Some(if existing.content.is_some() {
                            FileStatus::Known
                        } else {
                            FileStatus::Unknown
                        }),
                    })
                    .await?;
                if existing.content.is_none() {
                    // Classified as a new file is: the path, the NFOs beside
                    // it, and the tags this probe just read (issue #184).
                    let content = self
                        .classify_media_content(path, library, Some(duration), &tags)
                        .await?;
                    let status = if content.is_some() {
                        FileStatus::Known
                    } else {
                        FileStatus::Unknown
                    };
                    updated = self
                        .file_repo
                        .set_classification(
                            existing.id,
                            FileClassification {
                                content,
                                status,
                                classifier_version: CLASSIFIER_VERSION,
                            },
                        )
                        .await?;
                }
                self.check_and_report_duplicate(&updated).await;
                self.check_and_report_runtime_divergence(&updated).await;
                Ok(true)
            }
            Err(e) if !content_changed => {
                debug!(
                    path = %path.display(),
                    error = %e,
                    "a file whose probe failed before still does not probe"
                );
                // Its size, modification time or identity moved but its
                // content did not (a `touch`, a copy that kept the bytes):
                // record them, so the next visit sees the file as it is and
                // does not hash it again only to find the same content. A
                // row with no identity yet is given the file's.
                if !stat.is_recorded_by(existing)
                    || (existing.identity.is_none() && identity.is_some())
                {
                    self.file_repo
                        .update(UpdateMediaFile {
                            id: existing.id,
                            hash: None,
                            size_bytes: Some(size),
                            mtime,
                            identity,
                            probe: ProbeUpdate::Keep,
                            content: None,
                            status: None,
                        })
                        .await?;
                }
                Ok(false)
            }
            Err(e) => {
                warn!(
                    "Failed to re-extract metadata for changed file {}: {}",
                    path.display(),
                    e
                );
                let status = if existing.content.is_some() {
                    FileStatus::Changed
                } else {
                    FileStatus::Unknown
                };
                // The old content's streams and probe results describe a
                // file that is gone. Cleared, the row reads as unprobed, so
                // every later visit probes it again until a probe succeeds.
                self.stream_repo.delete_by_file_id(existing.id).await?;
                self.replace_chapters(existing.id, Vec::new()).await?;
                let updated = self
                    .file_repo
                    .update(UpdateMediaFile {
                        id: existing.id,
                        hash: Some(new_hash),
                        size_bytes: Some(size),
                        mtime,
                        identity,
                        probe: ProbeUpdate::Clear,
                        content: None,
                        status: Some(status),
                    })
                    .await?;
                self.check_and_report_duplicate(&updated).await;
                self.check_and_report_runtime_divergence(&updated).await;
                Ok(true)
            }
        }
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
            Some(lib_uuid),
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
            Some(lib_uuid),
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
    ///
    /// The identity passes run once, before any library is scanned (see
    /// [`Self::identity_passes_done`]); if they fail, every library is still
    /// scanned, without reclassifying files classified by older rules.
    ///
    /// A library whose scan job is already queued or running -- an
    /// administrator's scan, say -- is skipped: that scan covers it.
    pub async fn scan_all_libraries(&self, trigger: ScanTrigger) -> Result<u32, IndexError> {
        let reclassify = self.identity_passes_done().await;

        let libraries = self.library_repo.find_all().await?;
        let mut total_added: u64 = 0;
        for library in libraries {
            let ticket = match self.begin_scan(library.id, trigger).await {
                Ok(ticket) => ticket,
                Err(IndexError::ScanInProgress) => {
                    info!(
                        library_id = %library.id,
                        "a scan of this library is already queued or running; skipping it"
                    );
                    continue;
                }
                // Deleted since it was listed, or being deleted.
                Err(IndexError::LibraryNotFound) => {
                    info!(library_id = %library.id, "the library is being deleted; skipping it");
                    continue;
                }
                Err(e) => {
                    error!("Scan failed for library {}: {}", library.id, e);
                    continue;
                }
            };
            match self.run_registered_scan(ticket, reclassify).await {
                Ok(progress) => total_added += progress.added,
                Err(e) => error!("Scan failed for library {}: {}", library.id, e),
            }
        }
        Ok(u32::try_from(total_added).unwrap_or(u32::MAX))
    }

    /// Register a `trigger` scan of `library_id` and run it to the end on the
    /// caller's task. For the background tasks, which have nothing to answer
    /// before the scan runs.
    pub async fn scan_now(
        &self,
        library_id: Uuid,
        trigger: ScanTrigger,
    ) -> Result<ScanProgress, IndexError> {
        let ticket = self.begin_scan(library_id, trigger).await?;
        self.run_scan(ticket).await
    }

    /// Reconcile a single path in response to a filesystem-watcher event.
    ///
    /// Never waits for the library: while a scan job is queued or running
    /// for it, or another reconcile holds it, the event is handed back
    /// [`ReconcileOutcome::Deferred`] untouched -- as is an event for a file
    /// still being written (see [`Self::with_settle_window`]). The caller
    /// retries it after the delay the outcome names.
    ///
    /// `kind` is a hint only: what is at the path now decides.
    ///
    /// * **A file** is reconciled as a scan would: an indexed one against
    ///   its row, a new one indexed -- or, when it is a moved or renamed
    ///   file, relinked to its row (issue #180). A file whose content is
    ///   another row's that may have moved too -- one half of a swap or a
    ///   rotation -- is left as it is for the next scan, which sees every
    ///   path at once.
    /// * **A directory** -- one renamed, or moved into the library -- is
    ///   walked, and every file beneath it reconciled the same way. A row
    ///   beneath it the walk did not see is marked missing, as a scan would
    ///   mark it (see [`Self::reconcile_directory`]).
    /// * **Nothing** -- or a symlink, which is not part of the library
    ///   (issue #186) -- marks the row at the path missing. With no row
    ///   there the path was a directory: every row beneath it whose file is
    ///   gone is marked missing. The watcher never purges.
    ///
    /// A path at or inside a DVD or Blu-ray disc structure stands for its
    /// whole disc (issue #234): the disc is walked again as a directory is,
    /// or, when it is gone, its rows are marked missing.
    pub async fn reconcile_path(
        &self,
        library_id: Uuid,
        path: PathBuf,
        kind: FsEventKind,
    ) -> Result<ReconcileOutcome, IndexError> {
        // Ignore events for libraries that no longer exist.
        let Some(library) = self.library_repo.find_by_id(library_id).await? else {
            return Ok(ReconcileOutcome::Done);
        };

        let path_str = path.to_string_lossy().to_string();

        // Only a file awaiting reclassification needs the identity passes;
        // asking for any other would retry a failing pass on every watcher
        // event. They hold the catalog gate exclusively, so they are asked
        // for before this event takes the gate itself.
        let reclassify = match self.file_repo.find_by_path(&path_str).await? {
            Some(existing) if awaits_reclassification(&existing) => {
                self.identity_passes_done_now().await
            }
            _ => false,
        };

        let Some(_guard) = self.scans.try_acquire_for_reconcile(library_id) else {
            debug!(
                path = %path.display(),
                %library_id,
                "the library is being scanned; deferring the event"
            );
            return Ok(ReconcileOutcome::Deferred {
                retry_after: LIBRARY_BUSY_RETRY,
            });
        };
        debug!(path = %path.display(), ?kind, "reconciling a watcher event");

        // Only a stat that says "no such file" means the path is gone. Any
        // other failure (EACCES from an unsearchable parent, a transient EIO
        // or ESTALE on a network mount) says nothing about the file, so the
        // event is dropped rather than read as a deletion; the next scan
        // shields the same path (issue #179). The stat follows no link
        // beneath the root ([`StatCursor::kind`], issues #238 and #246): a
        // link is not a library file (issue #186), so a path that is now one
        // -- or is reached through a folder that is now one, which the stat
        // refuses -- reconciles exactly like a path that is gone.
        let kind = match StatCursor::new(&library.root_path).kind(&path) {
            Ok(EntryKind::Other) => None,
            Ok(kind) => Some(kind),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound || is_refusal(&err) => None,
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "could not stat a changed path; leaving its row as it is"
                );
                return Ok(ReconcileOutcome::Done);
            }
        };

        // Anything at or inside a disc structure is its disc's (issue #234):
        // which of its stream files the disc's main title plays depends on
        // all of them and on its IFOs or playlists, so the disc is
        // reconciled whole -- read again if it is there, and its rows marked
        // missing if it is not.
        if let Some((disc_root, _)) = self
            .path_policy
            .disc_root(relative_to(&library.root_path, &path))
        {
            let disc_root = library.root_path.join(disc_root);
            return match StatCursor::new(&library.root_path).kind(&disc_root) {
                Ok(EntryKind::Folder) => {
                    let inodes = self.inodes_of(&library);
                    self.reconcile_directory(&disc_root, &library, inodes).await
                }
                Ok(EntryKind::File | EntryKind::Other) => {
                    self.reconcile_gone(&disc_root, &library).await?;
                    self.rederive_disc_set_of(&library, &disc_root).await?;
                    Ok(ReconcileOutcome::Done)
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound || is_refusal(&err) => {
                    self.reconcile_gone(&disc_root, &library).await?;
                    self.rederive_disc_set_of(&library, &disc_root).await?;
                    Ok(ReconcileOutcome::Done)
                }
                Err(err) => {
                    warn!(
                        path = %disc_root.display(),
                        error = %err,
                        "could not stat a changed disc structure; leaving its rows as they are"
                    );
                    Ok(ReconcileOutcome::Done)
                }
            };
        }

        // A subtitle or an NFO beside the media (issue #184). Handled here,
        // before the file-row bookkeeping below -- which still marks missing a
        // row a build before issue #182 made of one.
        if self
            .path_policy
            .disposition(relative_to(&library.root_path, &path))
            == PathDisposition::Sidecar
        {
            let is_file = kind == Some(EntryKind::File);
            if hints::is_nfo(&path) {
                self.reconcile_nfo_event(&library, &path, is_file, &[])
                    .await?;
            } else {
                self.reconcile_sidecar_event(&library, &path, is_file)
                    .await?;
            }
        }

        let outcome = match kind {
            Some(EntryKind::File) => {
                let inodes = self.inodes_of(&library);
                self.reconcile_file(&path, &library, reclassify, inodes)
                    .await?
            }
            Some(EntryKind::Folder) => {
                let inodes = self.inodes_of(&library);
                return self.reconcile_directory(&path, &library, inodes).await;
            }
            Some(EntryKind::Other) | None => {
                self.reconcile_gone(&path, &library).await?;
                // A disc folder of a set gone takes its disc out of the set
                // (issue #234).
                self.rederive_disc_set_of(&library, &path).await?;
                return Ok(ReconcileOutcome::Done);
            }
        };
        Ok(match outcome {
            FileOutcome::Deferred(retry_after) => ReconcileOutcome::Deferred { retry_after },
            FileOutcome::Added
            | FileOutcome::Relinked
            | FileOutcome::Changed
            | FileOutcome::Unchanged
            | FileOutcome::LeftToScan
            | FileOutcome::Failed => ReconcileOutcome::Done,
        })
    }

    /// Reconcile the file at `path` for a watcher event: restore and
    /// reconcile its row, or index it -- relinking a moved file to its row.
    /// `inodes` is what the library's filesystem keeps.
    async fn reconcile_file(
        &self,
        path: &Path,
        library: &Library,
        reclassify: bool,
        inodes: Inodes,
    ) -> Result<FileOutcome, IndexError> {
        let path_str = path.to_string_lossy().to_string();
        // A file the policy keeps out of the library is not indexed. One that
        // was -- a `.nfo` from before issue #182, a sample renamed into place
        // -- is marked missing, exactly as a full scan would leave it. A
        // disc's stream file reaches here only from a walk of its disc,
        // which offers only those the disc's main title plays (issue #234).
        if !matches!(
            self.path_policy
                .disposition(relative_to(&library.root_path, path)),
            PathDisposition::Media | PathDisposition::DiscStream(_)
        ) {
            if let Some(file) = self.file_repo.find_by_path(&path_str).await?
                && file.missing_since.is_none()
            {
                info!("Marking excluded file missing: {}", path.display());
                self.file_repo
                    .mark_missing(vec![file.id], self.clock.now())
                    .await?;
            }
            return Ok(FileOutcome::Unchanged);
        }

        match self.file_repo.find_by_path(&path_str).await? {
            Some(existing) => {
                // New content that another row of the library records, where
                // that row may have moved: a swap, a rotation, a rename onto
                // this row's path. Which row is which needs every path at
                // once, so the next scan decides (issue #180); the row is not
                // even restored meanwhile.
                //
                // A path that is no longer a regular file reached without a
                // link is no file of the library: its row is marked missing,
                // and not restored first (issue #238).
                let found = match self
                    .fingerprint(&library.root_path, path, Some(&existing), inodes)
                    .await
                {
                    Ok(found) => found,
                    Err(err) if is_refusal(&err) => {
                        self.reconcile_gone(path, library).await?;
                        return Ok(FileOutcome::Unchanged);
                    }
                    Err(_) => None,
                };
                if let Some(found) = &found
                    && found.hash != existing.hash
                    && found.hash != 0
                    && self
                        .file_repo
                        .find_by_library_and_hash_including_missing(library.id, found.hash)
                        .await?
                        .iter()
                        .any(|row| {
                            row.id != existing.id
                                && row.size_bytes == found.size
                                && may_have_moved(&library.root_path, row, inodes)
                        })
                {
                    info!(
                        path = %path.display(),
                        "a file's new content matches a file of the library that may have moved; leaving it to the next scan"
                    );
                    return Ok(FileOutcome::LeftToScan);
                }
                self.restore_if_missing(&existing).await?;
                self.reconcile_existing_file(
                    &existing,
                    path,
                    library,
                    reclassify,
                    found.as_ref(),
                    &mut LibraryStats::new(&library.root_path, inodes),
                )
                .await
            }
            None => {
                let outcome = self
                    .process_new_file(path, library, RelinkSource::Repository, None, inodes)
                    .await?;
                match outcome {
                    FileOutcome::Added => {
                        record_file_outcome("new");
                        self.attach_adjacent_sidecars(library, path).await?;
                    }
                    // A moved video's subtitles are those beside it now
                    // (issue #184); its NFOs' records moved with it as it was
                    // relinked, and changes to them are their own events'.
                    FileOutcome::Relinked => self.relink_sidecars(library, path).await?,
                    FileOutcome::Changed
                    | FileOutcome::Unchanged
                    | FileOutcome::Deferred(_)
                    | FileOutcome::LeftToScan
                    | FileOutcome::Failed => {}
                }
                Ok(outcome)
            }
        }
    }

    /// Nothing is at `path` any more -- or only a symlink, which is not part
    /// of the library. Its row, if it has one, is marked missing; with none,
    /// `path` was a directory, and every row beneath it whose file is gone is
    /// marked missing (issue #180), and the record of every NFO beneath it
    /// forgotten, as a removed NFO's own event forgets its record (FR-219):
    /// Beam saw it gone, so one put back is applied again. The watcher never
    /// purges: that waits for a scan and the grace period.
    ///
    /// A path the policy never indexes anything beneath -- an excluded
    /// directory, a hidden or ignored name, a sidecar file -- is not looked
    /// beneath at all. Nor is a removed directory believed while the library
    /// root holds no video file: that is a volume going away, as the scan's
    /// empty-root guard reads it, and the next scan decides.
    async fn reconcile_gone(&self, path: &Path, library: &Library) -> Result<(), IndexError> {
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
        let now = self.clock.now();
        if let Some(file) = self.file_repo.find_by_path(&path.to_string_lossy()).await? {
            if file.missing_since.is_none() {
                info!("Marking deleted file missing: {}", path.display());
                self.file_repo.mark_missing(vec![file.id], now).await?;
            }
            return Ok(());
        }
        let rel = relative_to(&library.root_path, path);
        if self.path_policy.excludes_directory(rel)
            || self.path_policy.disposition(rel) == PathDisposition::Sidecar
        {
            return Ok(());
        }
        // Each file is asked after rather than assumed gone with its
        // directory: the directory may be gone while a file of it is not --
        // a rename the watcher reports as two events, the new name
        // reconciled first, has already moved the rows along. A file reached
        // only through a link -- the directory swapped for a link to one
        // holding files of the same names -- is gone too: it is no file of
        // the library (issue #238).
        let gone: Vec<MediaFile> = self
            .file_repo
            .find_beneath_including_missing(library.id, path)
            .await?
            .into_iter()
            .filter(|row| {
                row.missing_since.is_none()
                    && (path_is_absent(&library.root_path, &row.path)
                        || stat_regular_file(&library.root_path, &row.path)
                            .is_err_and(|err| is_refusal(&err)))
            })
            .collect();
        if !gone.is_empty() {
            let video_files_seen = usize::from(root_holds_a_video_file(
                &library.root_path,
                &self.path_policy,
            ));
            let indexed_video_files = gone.iter().filter(|row| is_video_path(&row.path)).count();
            if root_looks_unmounted(video_files_seen, indexed_video_files) {
                warn!(
                    path = %path.display(),
                    root = %library.root_path.display(),
                    "library root holds no video files; leaving a removed directory to the next scan"
                );
                return Ok(());
            }
        }
        if let Some(repo) = &self.applied_nfo_repo {
            let forgotten = repo.delete_beneath(library.id, path).await?;
            if forgotten > 0 {
                info!(
                    path = %path.display(),
                    forgotten,
                    "Forgot the NFOs of a removed directory"
                );
            }
        }
        if gone.is_empty() {
            return Ok(());
        }
        info!(
            path = %path.display(),
            files = gone.len(),
            "Marking the files of a removed directory missing"
        );
        self.file_repo
            .mark_missing(gone.into_iter().map(|row| row.id).collect(), now)
            .await?;
        Ok(())
    }

    /// Reconcile the directory at `dir` for a watcher event -- a directory
    /// renamed, or moved into the library (issue #180).
    ///
    /// Every media file beneath it is reconciled as a single file is: a file
    /// of a renamed directory is new at its path, and is relinked to its row
    /// by content. A row beneath `dir` the walk did not see is then marked
    /// missing, as a scan would mark it, unless the walk failed on a path
    /// above it; the watcher never purges. A directory the path policy
    /// excludes indexes nothing, so every row beneath it is marked missing.
    ///
    /// The library root itself is left to the scan: a root that reads as
    /// empty may be an unmounted volume, which only the scan's empty-root
    /// guard can tell. Deferred as a whole when any file beneath it is still
    /// being written; walking it again later is idempotent.
    async fn reconcile_directory(
        &self,
        dir: &Path,
        library: &Library,
        inodes: Inodes,
    ) -> Result<ReconcileOutcome, IndexError> {
        if dir == library.root_path {
            debug!(root = %dir.display(), "an event for the library root is left to the scan");
            return Ok(ReconcileOutcome::Done);
        }
        let WalkOutcome {
            files,
            video_files_seen: _,
            excluded: _,
            failed_subtrees,
            unscoped_failure,
            subtitles,
            nfos,
            discs,
        } = if self
            .path_policy
            .excludes_directory(relative_to(&library.root_path, dir))
        {
            WalkOutcome {
                files: Vec::new(),
                video_files_seen: 0,
                excluded: 0,
                failed_subtrees: Vec::new(),
                unscoped_failure: false,
                subtitles: Vec::new(),
                nfos: Vec::new(),
                discs: Vec::new(),
            }
        } else {
            walk_under(&library.root_path, dir, &self.path_policy)
        };

        let mut retry_after: Option<Duration> = None;
        let mut left_to_scan: Vec<PathBuf> = Vec::new();
        for path in &files {
            let outcome = match self.reconcile_file(path, library, false, inodes).await {
                Ok(outcome) => outcome,
                Err(e) => {
                    // One file that cannot be indexed does not stop the rest.
                    warn!(path = %path.display(), error = %e, "failed to reconcile a file of a changed directory");
                    record_file_outcome("failed");
                    FileOutcome::Failed
                }
            };
            match outcome {
                FileOutcome::Deferred(after) => {
                    retry_after = Some(retry_after.map_or(after, |before| before.min(after)));
                }
                FileOutcome::LeftToScan => left_to_scan.push(path.clone()),
                FileOutcome::Added
                | FileOutcome::Relinked
                | FileOutcome::Changed
                | FileOutcome::Unchanged
                | FileOutcome::Failed => {}
            }
        }
        // Each disc read's source, once its files are at their paths.
        self.rederive_disc_parts(library, &discs).await?;

        // What sits beside the media beneath it (issue #184), once its videos
        // are at their paths: each subtitle is recorded against the video
        // that owns it, and each NFO applied as its own event would apply it
        // -- a watcher that reports a renamed directory reports nothing of
        // the files inside it. A changed NFO beside a video left to the scan
        // is left to the scan with it (FR-219).
        for subtitle in &subtitles {
            if let Err(e) = self
                .reconcile_sidecar_event(library, &subtitle.path, true)
                .await
            {
                warn!(path = %subtitle.path.display(), error = %e, "failed to reconcile a subtitle of a changed directory");
            }
        }
        for nfo in &nfos {
            if let Err(e) = self
                .reconcile_nfo_event(library, &nfo.path, true, &left_to_scan)
                .await
            {
                warn!(path = %nfo.path.display(), error = %e, "failed to reconcile an NFO of a changed directory");
            }
        }

        // Read after the files are reconciled, so a row relinked to a path
        // beneath `dir` is seen at its new path. A row at `dir` itself is a
        // file a directory has replaced.
        let seen: std::collections::HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
        let mut rows = self
            .file_repo
            .find_beneath_including_missing(library.id, dir)
            .await?;
        if let Some(replaced) = self.file_repo.find_by_path(&dir.to_string_lossy()).await? {
            rows.push(replaced);
        }
        let now = self.clock.now();
        let MissingPlan {
            mark,
            purge: _,
            shielded,
        } = plan_missing(
            rows.iter().filter(|row| !seen.contains(row.path.as_path())),
            &failed_subtrees,
            unscoped_failure,
            now,
            // Never purged here: that is the scan's, after the grace period.
            Duration::MAX,
        );
        if shielded > 0 {
            warn!(
                dir = %dir.display(),
                shielded,
                "could not read part of a changed directory; rows beneath were left as they were"
            );
        }
        let marked = self.file_repo.mark_missing(mark, now).await?;
        if marked > 0 {
            info!(dir = %dir.display(), marked, "Marked files of a changed directory missing");
        }

        Ok(match retry_after {
            Some(retry_after) => ReconcileOutcome::Deferred { retry_after },
            None => ReconcileOutcome::Done,
        })
    }

    /// Tell the administrator a library's root is missing or not a directory.
    async fn report_root_unavailable(&self, library: &Library) {
        warn!(
            root = %library.root_path.display(),
            library_id = %library.id,
            "library root is not a directory"
        );
        self.notification_service.publish(AdminEvent::error(
            EventCategory::LibraryScan,
            format!(
                "Library '{}' root path does not exist or is not a directory: {}",
                library.name,
                library.root_path.display()
            ),
            Some(library.id),
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
                    "library_id": library.id.to_string(),
                    "path": library.root_path
                })),
            )
            .await;
    }

    /// Publish one scan-progress event (FR-208).
    fn publish_scan_event(
        &self,
        library: &Library,
        job_id: Uuid,
        phase: ScanPhase,
        progress: ScanProgress,
    ) {
        let name = &library.name;
        let message = match phase {
            ScanPhase::Started => format!("Scanning '{name}'"),
            ScanPhase::Progress => match progress.total {
                Some(total) => {
                    format!("Scanning '{name}': {} of {total} files", progress.processed)
                }
                None => format!("Scanning '{name}'"),
            },
            ScanPhase::Completed => format!("Scan of '{name}' finished"),
            ScanPhase::Failed => format!("Scan of '{name}' failed"),
        };
        let library_id = Some(library.id);
        let library_name = Some(name.clone());
        let event = match phase {
            ScanPhase::Failed => AdminEvent::error(
                EventCategory::ScanProgress,
                message,
                library_id,
                library_name,
            ),
            ScanPhase::Started | ScanPhase::Progress | ScanPhase::Completed => AdminEvent::info(
                EventCategory::ScanProgress,
                message,
                library_id,
                library_name,
            ),
        };
        self.notification_service
            .publish(event.with_scan(ScanEvent {
                job_id,
                phase,
                progress,
            }));
    }

    /// Run a registered scan: wait for the library's lock, then scan it,
    /// keeping the job and the progress events current.
    async fn run_registered_scan(
        &self,
        ticket: ScanTicket,
        reclassify: bool,
    ) -> Result<ScanProgress, IndexError> {
        let library_id = ticket.library_id();
        let _guard = self.scans.acquire_for_scan(library_id).await;
        // Cancelled while it was queued: the job has already failed as
        // cancelled, and its library is being deleted.
        if ticket.is_cancelled() {
            return Err(IndexError::Cancelled);
        }
        ticket.start();

        let (library, result) = match self.library_repo.find_by_id(library_id).await {
            Ok(Some(library)) => {
                self.publish_scan_event(
                    &library,
                    ticket.job_id(),
                    ScanPhase::Started,
                    ScanProgress::default(),
                );
                let result = self.scan_one_library(&library, &ticket, reclassify).await;
                (Some(library), result)
            }
            Ok(None) => (None, Err(IndexError::LibraryNotFound)),
            Err(e) => (None, Err(IndexError::from(e))),
        };

        match result {
            Ok(progress) => {
                ticket.succeed(progress);
                if let Some(library) = &library {
                    self.publish_scan_event(
                        library,
                        ticket.job_id(),
                        ScanPhase::Completed,
                        progress,
                    );
                }
                Ok(progress)
            }
            Err(e) => {
                let progress = ticket.job().progress;
                ticket.fail(e.job_failure());
                if let Some(library) = &library {
                    self.publish_scan_event(library, ticket.job_id(), ScanPhase::Failed, progress);
                }
                Err(e)
            }
        }
    }
}

/// The error a scan of a library whose root is not a directory ends in. Its
/// message names no path (NFR-108).
fn root_unavailable() -> IndexError {
    IndexError::PathNotFound("Library root path does not exist or is not a directory".to_string())
}

#[async_trait::async_trait]
impl IndexService for LocalIndexService {
    /// The root is checked here as well as when the scan runs, so an
    /// administrator asking for a scan of an unmounted root is told at once.
    async fn begin_scan(
        &self,
        library_id: Uuid,
        trigger: ScanTrigger,
    ) -> Result<ScanTicket, IndexError> {
        let library = self
            .library_repo
            .find_by_id(library_id)
            .await?
            .ok_or(IndexError::LibraryNotFound)?;
        if !library.root_path.is_dir() {
            self.report_root_unavailable(&library).await;
            return Err(root_unavailable());
        }
        let job = ScanJob {
            id: self.id_generator.new_id(),
            library_id,
            trigger,
            state: ScanState::Queued,
            queued_at: self.clock.now(),
            started_at: None,
            finished_at: None,
            progress: ScanProgress::default(),
            failure: None,
        };
        Ok(self.scans.register(job, self.clock.clone())?)
    }

    /// The identity passes run first if they have not succeeded yet, and
    /// until they have, files classified by older rules are not reclassified
    /// (see [`LocalIndexService::identity_passes_done`]).
    async fn run_scan(&self, ticket: ScanTicket) -> Result<ScanProgress, IndexError> {
        let reclassify = self.identity_passes_done().await;
        self.run_registered_scan(ticket, reclassify).await
    }

    fn scan_job(&self, library_id: Uuid) -> Option<ScanJob> {
        self.scans.job(library_id)
    }

    fn subscribe_scan(&self, library_id: Uuid) -> tokio::sync::watch::Receiver<Option<ScanJob>> {
        self.scans.subscribe(library_id)
    }

    async fn stop_scan(&self, library_id: Uuid) -> StoppedScan {
        // Retired first, so nothing registers between the cancel and the
        // caller's delete of the library.
        let retirement = self.scans.retire(library_id);
        if !self.scans.cancel(library_id, self.clock.now()) {
            return StoppedScan {
                in_time: true,
                retirement,
            };
        }
        // Subscribed after the cancel: `wait_for` reads the current job
        // first, so a scan that finished in between is seen as finished.
        let mut jobs = self.scans.subscribe(library_id);
        let finished = async move {
            // An error is a closed channel: the slot is gone, and its job
            // with it. Either way nothing of this library's is running.
            let _ = jobs
                .wait_for(|job| !job.as_ref().is_some_and(|job| job.state.is_active()))
                .await;
        };
        let in_time = tokio::select! {
            () = finished => true,
            () = self.clock.sleep(SCAN_STOP_TIMEOUT) => false,
        };
        StoppedScan {
            in_time,
            retirement,
        }
    }

    fn forget_library(&self, library_id: Uuid) {
        self.scans.forget(library_id);
    }
}

#[cfg(test)]
impl LocalIndexService {
    /// The administrator's scan, run to the end on the caller's task and
    /// reported as the number of files it added: the shape the tests of the
    /// scan itself were written against before scans became jobs.
    pub(crate) async fn scan_library(&self, library_id: Uuid) -> Result<u32, IndexError> {
        let progress = self.scan_now(library_id, ScanTrigger::Manual).await?;
        Ok(u32::try_from(progress.added).expect("a test scan adds few files"))
    }
}

impl LocalIndexService {
    /// Hash every walked path whose content may have moved to or from it --
    /// a new path, or an indexed one its row no longer records as it is
    /// ([`FileStat::is_recorded_by`]) -- when some row's content may have
    /// left its path: a row the walk did not see, or one whose path
    /// changed. With neither, nothing moved, and
    /// each file is hashed, if at all, when it is reconciled; with both, the
    /// hash taken here is the one reconciling it reuses. `walked_stats` is
    /// each walked file's stat as [`stat_walked`] read it; a file with none is
    /// not hashed here, and each is hashed on the filesystem its stat was.
    async fn fingerprint_walk(
        &self,
        root: &Path,
        walked_files: &[PathBuf],
        walked_stats: &HashMap<PathBuf, FileStat>,
        rows: &HashMap<PathBuf, MediaFile>,
        is_shielded: &impl Fn(&Path) -> bool,
        ticket: &ScanTicket,
    ) -> Result<HashMap<PathBuf, Fingerprint>, IndexError> {
        let mut changed = 0usize;
        let mut to_hash: Vec<(&PathBuf, Inodes)> = Vec::new();
        for path in walked_files {
            let row = rows.get(path);
            let Some(stat) = walked_stats.get(path) else {
                continue;
            };
            match row {
                Some(row) if stat.is_recorded_by(row) => continue,
                Some(_) => changed += 1,
                None => {}
            }
            to_hash.push((path, stat.inodes));
        }
        let walked: std::collections::HashSet<&Path> =
            walked_files.iter().map(PathBuf::as_path).collect();
        let gone = rows.values().any(|row| {
            row.hash != 0 && !walked.contains(row.path.as_path()) && !is_shielded(&row.path)
        });
        let mut fingerprints = HashMap::new();
        if !(gone || changed > 0) {
            return Ok(fingerprints);
        }
        for (path, inodes) in to_hash {
            if ticket.is_cancelled() {
                info!("Scan cancelled");
                return Err(IndexError::Cancelled);
            }
            if let Ok(Some(found)) = self.fingerprint(root, path, rows.get(path), inodes).await {
                fingerprints.insert(path.clone(), found);
            }
        }
        Ok(fingerprints)
    }

    /// Scan one library under a registered job, reclassifying files
    /// classified by older rules only when `reclassify` is set. The caller
    /// holds the library's lock and the catalog gate.
    async fn scan_one_library(
        &self,
        library: &Library,
        ticket: &ScanTicket,
        reclassify: bool,
    ) -> Result<ScanProgress, IndexError> {
        let lib_uuid = library.id;
        let library_id = lib_uuid.to_string();
        let start_time = self.clock.now();
        let mut progress = ScanProgress::default();
        let mut throttle = ProgressThrottle::new(PROGRESS_EVENT_INTERVAL);

        // Cancelled while it waited for the library -- its library is being
        // deleted -- so there is nothing to scan.
        if ticket.is_cancelled() {
            return Err(IndexError::Cancelled);
        }

        info!(
            "Scanning library: {} ({:?})",
            library.name, library.root_path
        );

        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            format!("Library scan started for '{}'", library.name),
            Some(lib_uuid),
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
            self.report_root_unavailable(library).await;
            return Err(root_unavailable());
        }

        // Phase 1: Fetch existing files from DB -- missing ones included, so a
        // path that comes back is matched to its old row (and id), and so the
        // empty-root guard below still counts a row that is already missing.
        // Nothing else writes this library's rows while the scan holds its
        // lock, so the snapshot -- `missing_since` stamps included -- stays
        // true until phase 4 reads it.
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
            video_files_seen: walked_video_files,
            excluded: excluded_count,
            failed_subtrees,
            unscoped_failure,
            subtitles: walked_subtitles,
            nfos: walked_nfos,
            discs: walked_discs,
        } = walk_library_root(&library.root_path, &self.path_policy);
        let inodes = self.inodes_of(library);

        // An unmounted volume usually leaves its mount point behind as an empty
        // directory, which passes the guard above -- or one holding only a
        // sentinel or hidden file (`.not_mounted`, `.DS_Store`, `Thumbs.db`),
        // which admins put on mount points on purpose. Reconciling that walk
        // would mark every indexed video row missing, so a root with no video files
        // under a library that has indexed video files is refused rather than
        // believed. Only video rows are counted on either side: they are what
        // is at stake, and a library that only ever held non-video files has
        // nothing an unmounted volume could take from it. Emptying a library on
        // purpose is deleting the library. The walk counts video files the path
        // policy excludes too: a root of samples is mounted (issue #182).
        let indexed_video_files = existing_map
            .keys()
            .filter(|path| is_video_path(path))
            .count();
        if root_looks_unmounted(walked_video_files, indexed_video_files) {
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
                Some(lib_uuid),
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

        // Every walked file stat'ed once, without opening it. One that has
        // become a link since the walk is no file of the library any more,
        // so the files to index -- the scan's total -- may be fewer than the
        // walk found.
        let (walked_files, walked_stats) =
            stat_walked(&library.root_path, walked_files, inodes, ticket)?;
        let walked_media = walked_files.clone();
        progress.total = Some(walked_files.len() as u64);
        ticket.record(progress);

        // Phase 3a: find where content went (issue #180). A row whose file
        // was moved, renamed, swapped with another or rotated among others
        // follows its content to the path that holds it now, keeping its id;
        // it is not left missing beside a new row, nor its path's new
        // content read as a change to it.
        let walked: std::collections::HashSet<&Path> =
            walked_files.iter().map(PathBuf::as_path).collect();
        let is_shielded = |path: &Path| {
            unscoped_failure
                || failed_subtrees
                    .iter()
                    .any(|failed| path.starts_with(failed))
        };
        let fingerprints = self
            .fingerprint_walk(
                &library.root_path,
                &walked_files,
                &walked_stats,
                &existing_map,
                &is_shielded,
                ticket,
            )
            .await?;
        let matches = content_matches(&existing_map, &walked, &fingerprints, is_shielded);
        // The rows that may be paired, and the rows at the paths they may
        // be paired with: whether a pairing is a replace-by-rename turns on
        // whether each was played.
        let tied: Vec<MediaFile> = matches
            .iter()
            .flat_map(|m| std::iter::once(m.row).chain(existing_map.get(m.path)))
            .cloned()
            .collect();
        let last_played = self.last_played(&tied).await?;
        let ContentMoves { relinks, displaced } =
            plan_content_moves(&existing_map, matches, &last_played);
        drop(walked);
        let mut relinked_to: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut displaced_marked = 0u64;
        if !relinks.is_empty() {
            self.relink_files(&relinks, &displaced, library).await?;
            for (row, path, _) in &relinks {
                existing_map.remove(&row.path);
                relinked_to.insert(path.clone());
            }
            for row in &displaced {
                existing_map.remove(&row.path);
                if row.missing_since.is_none() {
                    displaced_marked += 1;
                }
            }
        }

        // Phase 3b: Compare with DB, add new files
        let mut stats = LibraryStats::new(&library.root_path, inodes);
        for path in &walked_files {
            if ticket.is_cancelled() {
                info!(library_id = %lib_uuid, "Scan cancelled");
                return Err(IndexError::Cancelled);
            }
            let known = fingerprints.get(path);
            let outcome = if relinked_to.contains(path) {
                FileOutcome::Relinked
            } else if let Some(existing_file) = existing_map.remove(path) {
                // Known file: bring it back if it was missing, then reconcile
                // it against its current on-disk state.
                let reconciled = match self.restore_if_missing(&existing_file).await {
                    Ok(restored) => {
                        if restored {
                            progress.restored += 1;
                        }
                        self.reconcile_existing_file(
                            &existing_file,
                            path,
                            library,
                            reclassify,
                            known,
                            &mut stats,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                };
                match reconciled {
                    Ok(outcome) => outcome,
                    Err(e) => {
                        self.report_file_failure(lib_uuid, &library.name, path, &e)
                            .await;
                        FileOutcome::Failed
                    }
                }
            } else {
                // A new file: phase 3a relinked every moved one it found.
                match self
                    .process_new_file(path, library, RelinkSource::Planned, known, inodes)
                    .await
                {
                    Ok(outcome) => {
                        if outcome == FileOutcome::Added {
                            record_file_outcome("new");
                        }
                        outcome
                    }
                    Err(e) => {
                        self.report_file_failure(lib_uuid, &library.name, path, &e)
                            .await;
                        FileOutcome::Failed
                    }
                }
            };
            match outcome {
                FileOutcome::Added => progress.added += 1,
                FileOutcome::Relinked => progress.relinked += 1,
                FileOutcome::Changed => progress.changed += 1,
                FileOutcome::Unchanged => progress.unchanged += 1,
                // Seen, so never marked missing: a file still being copied in
                // is on disk. The watcher, or the next scan, comes back to it.
                // A scan never leaves a file to itself; were it to, it would
                // be the same wait.
                FileOutcome::Deferred(_) | FileOutcome::LeftToScan => progress.deferred += 1,
                FileOutcome::Failed => progress.failed += 1,
            }
            progress.processed += 1;
            ticket.record(progress);
            if throttle.ready(self.clock.monotonic()) {
                self.publish_scan_event(library, ticket.job_id(), ScanPhase::Progress, progress);
            }
        }

        // A library deleted during its last file is not reconciled against a
        // walk of a root it no longer owns.
        if ticket.is_cancelled() {
            info!(library_id = %lib_uuid, "Scan cancelled");
            return Err(IndexError::Cancelled);
        }

        // Each disc's files take the part their place in its source makes
        // them (issue #234): a row the loop above left unchanged may be one
        // whose disc's main title changed around it.
        if let Err(e) = self.rederive_disc_parts(library, &walked_discs).await {
            error!(library_id = %lib_uuid, error = %e, "re-deriving the parts of disc files failed");
        }

        // Phase 3c: What sits beside the media (issue #184). The NFOs whose
        // content changed since they were last applied re-pin their titles,
        // and every subtitle is recorded against the video that owns it --
        // after phase 3a, so a relinked video's are judged at its new path. A
        // failure here is the scan's, like a failure to record a file: it is
        // reported and the scan goes on.
        if let Err(e) = self
            .reapply_changed_nfos(library, &walked_nfos, &failed_subtrees, unscoped_failure)
            .await
        {
            error!(library_id = %lib_uuid, error = %e, "re-applying changed NFOs failed");
        }
        if let Err(e) = self
            .reconcile_sidecars(
                library,
                &walked_media,
                &walked_subtitles,
                &failed_subtrees,
                unscoped_failure,
            )
            .await
        {
            error!(library_id = %lib_uuid, error = %e, "reconciling sidecar subtitles failed");
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
        // A displaced row was stamped missing as it was moved aside.
        let marked_count = displaced_marked + self.file_repo.mark_missing(mark, now).await?;
        let purged_count = self.file_repo.purge_missing(purge).await?;
        progress.marked_missing = marked_count;
        progress.purged = purged_count;
        ticket.record(progress);
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

        // Phase 5: Retire the titles no file row is left for -- a movie or
        // show whose files have all been purged, now or by an earlier scan
        // (issue #183). A title whose files are only soft-deleted keeps them,
        // and so is kept; it is merely hidden from browse. Only a walk that
        // read the whole tree gets here: one that failed anywhere has told us
        // nothing reliable about what is on disk. `start_time` protects a
        // title another library's scan or reconcile created while this scan
        // ran, whose file row may not be written yet. It does not protect a
        // title already orphaned before the scan began that another library
        // is attaching a file to: that title can go under it, and the next
        // scan of that library re-indexes the file.
        let titles_removed = if failed_subtrees.is_empty() && !unscoped_failure {
            self.movie_repo.delete_orphaned(start_time).await?
                + self.show_repo.delete_orphaned(start_time).await?
        } else {
            0
        };
        if titles_removed > 0 {
            info!("Removed {} titles with no files left", titles_removed);
        }

        // Update scan finish time
        let end_time = self.clock.now();
        let total_files = self.library_repo.count_files(lib_uuid).await?;

        self.library_repo
            .update_scan_progress(lib_uuid, None, Some(end_time), Some(total_files as i32))
            .await?;

        let ScanProgress {
            total: _,
            processed: _,
            added: added_count,
            changed: _,
            unchanged: _,
            deferred: deferred_count,
            failed: _,
            marked_missing: _,
            restored: restored_count,
            relinked: relinked_count,
            purged: _,
        } = progress;
        info!(
            "Scan complete. Added: {}, Relinked: {}, Deferred: {}, Marked missing: {}, Restored: {}, Purged: {}, Total: {}",
            added_count,
            relinked_count,
            deferred_count,
            marked_count,
            restored_count,
            purged_count,
            total_files
        );

        self.notification_service.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            format!(
                "Library scan complete for '{}': added {}, moved {}, missing {}, restored {}, purged {}, total {}",
                library.name,
                added_count,
                relinked_count,
                marked_count,
                restored_count,
                purged_count,
                total_files
            ),
            Some(lib_uuid),
            Some(library.name.clone()),
        ));
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                format!(
                    "Library scan completed: \"{}\" — {} added, {} moved, {} marked missing, {} restored, {} purged, {} total",
                    library.name,
                    added_count,
                    relinked_count,
                    marked_count,
                    restored_count,
                    purged_count,
                    total_files
                ),
                Some(serde_json::json!({
                    "library_id": library_id,
                    "added": added_count,
                    "relinked": relinked_count,
                    "deferred": deferred_count,
                    "marked_missing": marked_count,
                    "restored": restored_count,
                    "purged": purged_count,
                    "excluded": excluded_count,
                    "titles_removed": titles_removed,
                    "total": total_files,
                })),
            )
            .await;

        Ok(progress)
    }
}

#[path = "index_hints.rs"]
mod hints;

#[path = "index_walk.rs"]
mod walk;

#[path = "index_relink.rs"]
mod relink;

#[path = "index_identity.rs"]
mod identity;

#[path = "index_divergence.rs"]
mod divergence;

use divergence::*;
use identity::*;
use relink::*;
use walk::*;

#[path = "index_sidecars.rs"]
mod sidecars;

#[cfg(test)]
#[path = "index_missing_tests.rs"]
mod missing_tests;

#[cfg(test)]
#[path = "index_nfo_sidecar_tests.rs"]
mod nfo_sidecar_tests;

#[cfg(test)]
#[path = "index_identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "index_inference_tests.rs"]
mod inference_tests;

#[cfg(test)]
#[path = "index_relink_tests.rs"]
mod relink_tests;

#[cfg(test)]
#[path = "index_scan_tests.rs"]
mod scan_tests;

#[cfg(test)]
#[path = "index_no_follow_tests.rs"]
mod no_follow_tests;

#[cfg(test)]
#[path = "index_disc_tests.rs"]
mod disc_tests;

#[cfg(test)]
#[path = "index_test_harness.rs"]
mod test_harness;

#[cfg(test)]
#[path = "index_streams_tests.rs"]
mod streams_tests;

#[cfg(test)]
#[path = "index_classify_tests.rs"]
mod classify_tests;

#[cfg(test)]
#[path = "index_process_tests.rs"]
mod process_tests;

#[cfg(test)]
#[path = "index_divergence_tests.rs"]
mod divergence_tests;
