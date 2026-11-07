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

    /// A library rooted at `root`, for calling a per-file method directly.
    fn test_library(id: Uuid, root: &Path) -> Library {
        Library {
            id,
            name: "Test".to_string(),
            root_path: root.to_path_buf(),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        }
    }

    impl LocalIndexService {
        /// Classify `path` in a library rooted at its first component
        /// (`/media` for `/media/Show/x.mkv`; nothing for a bare filename),
        /// expecting a classification.
        async fn classify_for_test(
            &self,
            path: &Path,
            lib_id: Uuid,
            runtime: Duration,
        ) -> Result<MediaFileContent, IndexError> {
            let root: PathBuf = path.components().take(2).collect();
            let root = if path.is_absolute() {
                root
            } else {
                PathBuf::new()
            };
            let content = self
                .classify_media_content(
                    path,
                    &test_library(lib_id, &root),
                    Some(runtime),
                    &ContainerTags::default(),
                )
                .await?;
            Ok(content.expect("the path classifies"))
        }
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            codec_name: "subrip".to_string(),
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
            chapters: Vec::new(),
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

    /// The stored codec is the prober's FFmpeg name, for an image-based
    /// subtitle as for any other (#189): a PGS track used to read `Other(..)`.
    /// A stream flagged for the deaf and hard of hearing keeps the flag.
    #[tokio::test]
    async fn test_insert_subtitle_stream_records_ffmpeg_codec_name_and_sdh() {
        let repo = Arc::new(InMemoryMediaStreamRepository::default());
        let service = make_service_with_stream_repo(Arc::clone(&repo));
        let file_id = Uuid::new_v4();

        let mut pgs = make_subtitle_stream(0, Some("eng"), None);
        if let UtilStreamMetadata::Subtitle(s) = &mut pgs {
            s.codec_id = CodecId::Other("hdmv_pgs_subtitle".to_string());
            s.codec_name = "hdmv_pgs_subtitle".to_string();
            s.disposition =
                Disposition::from(ffmpeg_next::format::stream::Disposition::HEARING_IMPAIRED);
        }
        let plain = make_subtitle_stream(1, Some("eng"), None);
        let metadata = make_stream_file_metadata(vec![pgs, plain]);

        service
            .insert_media_streams(file_id, &metadata)
            .await
            .unwrap();

        let streams = repo.find_by_file_id(file_id).await.unwrap();
        let sdh = |stream: &beam_domain::models::MediaStream| match &stream.metadata {
            beam_domain::models::stream::StreamMetadata::Subtitle(sub) => sub.is_hearing_impaired,
            other => panic!("expected Subtitle metadata, got {other:?}"),
        };
        assert_eq!(streams[0].codec, "hdmv_pgs_subtitle");
        assert!(sdh(&streams[0]));
        assert_eq!(streams[1].codec, "subrip");
        assert!(!sdh(&streams[1]));
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
        let path = PathBuf::from("/media/Breaking Bad/Breaking.Bad.S01E02.mkv");

        let content = service
            .classify_for_test(&path, lib_id, Duration::from_secs(3600))
            .await
            .unwrap();

        let episode_id = match content {
            MediaFileContent::Episode { episode_id, .. } => episode_id,
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
            .classify_for_test(&path, lib_id, Duration::from_secs(1800))
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
            .classify_for_test(&path, lib_id, Duration::from_secs(2700))
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
        let path = PathBuf::from("/media/Breaking Bad/S03E05.mkv");

        service
            .classify_for_test(&path, lib_id, Duration::from_secs(3000))
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
            .classify_for_test(
                &PathBuf::from("/media/My Show/My.Show.S01E01.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        // Second call with same parent dir name — must reuse the existing show
        service
            .classify_for_test(
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
            .classify_for_test(
                &PathBuf::from("/media/Show/ep.S01E01.mkv"),
                lib_id,
                duration,
            )
            .await
            .unwrap();

        service
            .classify_for_test(
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

    // ─── several files for one episode (#142) ─────────────────────────────────

    /// A scan service over in-memory stores, with every probe succeeding and a
    /// distinct hash per file so no two files look like duplicate content.
    fn make_multi_source_scan_service(
        lib_repo: Arc<InMemoryLibraryRepository>,
        file_repo: Arc<InMemoryFileRepository>,
        show_repo: Arc<InMemoryShowRepository>,
    ) -> LocalIndexService {
        let next_hash = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .returning(move |_| Ok(next_hash.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .returning(|_| Ok(make_video_metadata()));
        LocalIndexService::new(
            lib_repo,
            file_repo,
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        )
    }

    fn episode_id_of(file: &MediaFile) -> Uuid {
        match file.content {
            Some(MediaFileContent::Episode { episode_id, .. }) => episode_id,
            ref other => panic!("expected an episode file, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_rips_of_one_episode_become_two_sources_of_one_episode() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;
        let show_dir = dir.path().join("The Show");
        std::fs::create_dir_all(&show_dir).unwrap();
        std::fs::write(show_dir.join("The.Show.S01E01.1080p.mkv"), b"1080p rip").unwrap();
        std::fs::write(show_dir.join("The.Show.S01E01.720p.mkv"), b"720p rip").unwrap();

        let service =
            make_multi_source_scan_service(lib_repo, file_repo.clone(), show_repo.clone());
        let indexed = service.scan_library(library.id).await.unwrap();

        assert_eq!(indexed, 2, "neither file is rejected by the scan");
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 2);
        let episode_id = episode_id_of(&files[0]);
        assert_eq!(
            episode_id_of(&files[1]),
            episode_id,
            "both files attach to the one S01E01"
        );
        assert_eq!(
            file_repo
                .find_by_episode_id(episode_id)
                .await
                .unwrap()
                .len(),
            2,
            "the episode has two sources"
        );
        assert_eq!(show_repo.episodes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_new_file_for_an_existing_episode_attaches_without_rewriting_it() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // An episode that already exists with no file behind it -- its old
        // file was replaced -- carrying a title enrichment established.
        let show = show_repo
            .find_or_create_by_identity(beam_domain::models::CreateShow::new(
                "Severance".to_string(),
                None,
            ))
            .await
            .unwrap();
        let season = show_repo.find_or_create_season(show.id, 1).await.unwrap();
        let existing = show_repo
            .find_or_create_episode(beam_domain::models::CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "Good News About Hell".to_string(),
                runtime: Some(Duration::from_secs(57 * 60)),
                air_date: None,
            })
            .await
            .unwrap();

        let show_dir = dir.path().join("Severance");
        std::fs::create_dir_all(&show_dir).unwrap();
        std::fs::write(
            show_dir.join("Severance.S01E01.REPACK.2160p.mkv"),
            b"new rip",
        )
        .unwrap();

        let service =
            make_multi_source_scan_service(lib_repo, file_repo.clone(), show_repo.clone());
        let indexed = service.scan_library(library.id).await.unwrap();

        assert_eq!(indexed, 1);
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            episode_id_of(&files[0]),
            existing.id,
            "the differently named file attaches to the existing episode"
        );
        let stored = show_repo
            .find_episode_by_id(existing.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.title, "Good News About Hell",
            "the new file's filename does not overwrite the established title"
        );
        assert_eq!(stored.runtime, Some(Duration::from_secs(57 * 60)));
        assert_eq!(show_repo.episodes.lock().unwrap().len(), 1);
    }

    // ─── classify_media_content: movie tests ──────────────────────────────────

    #[tokio::test]
    async fn test_classify_movie_simple_title() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        let path = PathBuf::from("/media/movies/Avatar.mp4");

        let content = service
            .classify_for_test(&path, lib_id, Duration::from_secs(9600))
            .await
            .unwrap();

        let entry_id = match content {
            MediaFileContent::Movie { movie_entry_id, .. } => movie_entry_id,
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
            .classify_for_test(&path, lib_id, Duration::from_secs(7200))
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
            .classify_for_test(&path, lib_id, Duration::from_secs(6000))
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
            .classify_for_test(&PathBuf::from("/media/Avatar.mp4"), lib_id, duration)
            .await
            .unwrap();

        // Second call with the same title — must reuse the existing movie record
        service
            .classify_for_test(&PathBuf::from("/backup/Avatar.mp4"), lib_id, duration)
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

        // Both copies are files of the one entry for the film's default
        // edition in this library (issue #182), not an entry each.
        let entries: Vec<_> = movie_repo
            .entries
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        assert_eq!(entries.len(), 1);
    }

    // ─── classify_media_content: edge cases ───────────────────────────────────

    #[tokio::test]
    async fn test_classify_empty_file_stem_falls_to_movie() {
        let (service, movie_repo, _) = make_classify_service();
        let lib_id = Uuid::new_v4();
        // Root path has no file-stem component — file_stem() returns None → empty string
        let path = PathBuf::from("/");

        let content = service
            .classify_for_test(&path, lib_id, Duration::from_secs(100))
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
            .classify_for_test(&path, lib_id, Duration::from_secs(3600))
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
                    chapters: Vec::new(),
                })
            });

        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(12345));

        let movie_id = Uuid::new_v4();
        mock_movie_repo
            .expect_find_or_create_by_identity()
            .times(1)
            .returning(move |_| {
                Ok(beam_domain::models::Movie {
                    id: movie_id,
                    title: "Avatar".to_string(),
                    identity_key: None,
                    pinned_ref: None,
                    pin_source: None,
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
                    content_rating: None,
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
            .expect_find_or_create_entry()
            .times(1)
            .returning(move |_| {
                Ok(beam_domain::models::MovieEntry {
                    id: entry_id,
                    library_id: Uuid::new_v4(),
                    movie_id: Uuid::new_v4(),
                    edition: None,
                    created_at: chrono::Utc::now(),
                })
            });

        mock_file_repo
            .expect_find_by_hash()
            .times(1)
            .returning(|_| Ok(vec![]));
        // No row of the library has the file's content: not a moved file.
        mock_file_repo
            .expect_find_by_library_and_hash_including_missing()
            .times(1)
            .returning(|_, _| Ok(vec![]));

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(beam_domain::models::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 12345,
                size_bytes: 1024,
                mtime: None,
                identity: None,
                mime_type: Some("video/mp4".to_string()),
                duration: None,
                container_format: None,
                content: Some(beam_domain::models::MediaFileContent::movie(entry_id)),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service
            .process_new_file(
                &path,
                &test_library(lib_id, temp_dir.path()),
                RelinkSource::Repository,
                None,
                Inodes::Stable,
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), FileOutcome::Added);
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
                    chapters: Vec::new(),
                })
            });

        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(67890));

        let show_id = Uuid::new_v4();
        mock_show_repo
            .expect_find_or_create_by_identity()
            .times(1)
            .returning(move |_| {
                Ok(beam_domain::models::Show {
                    id: show_id,
                    title: "Season 1".to_string(),
                    identity_key: None,
                    pinned_ref: None,
                    pin_source: None,
                    title_localized: None,
                    description: None,
                    year: None,
                    poster_url: None,
                    backdrop_url: None,
                    tmdb_id: None,
                    imdb_id: None,
                    tvdb_id: None,
                    anilist_id: None,
                    rating_tmdb: None,
                    content_rating: None,
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
            .expect_find_or_create_episode()
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
        // No row of the library has the file's content: not a moved file.
        mock_file_repo
            .expect_find_by_library_and_hash_including_missing()
            .times(1)
            .returning(|_, _| Ok(vec![]));

        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(beam_domain::models::MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 67890,
                size_bytes: 500 * 1024 * 1024,
                mtime: None,
                identity: None,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: None,
                content: Some(beam_domain::models::MediaFileContent::episode(episode_id)),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service
            .process_new_file(
                &path,
                &test_library(lib_id, temp_dir.path()),
                RelinkSource::Repository,
                None,
                Inodes::Stable,
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), FileOutcome::Added);
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let err = service
            .process_new_file(
                &path,
                &test_library(Uuid::new_v4(), temp_dir.path()),
                RelinkSource::Repository,
                None,
                Inodes::Stable,
            )
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

        // No prober expectation: a file is hashed before it is probed, so a
        // hash failure is reported without a probe.
        let mock_media_info = MockMediaInfoService::new();

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let err = service
            .process_new_file(
                &path,
                &test_library(Uuid::new_v4(), temp_dir.path()),
                RelinkSource::Repository,
                None,
                Inodes::Stable,
            )
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
            chapters: Vec::new(),
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Known);
    }

    /// Only media is indexed (issue #182): a sidecar, a stray text file and
    /// a sample are neither hashed nor probed nor given a row.
    #[tokio::test]
    async fn test_scan_library_indexes_only_media() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let movie = dir.path().join("Movie (2019)");
        std::fs::create_dir_all(&movie).unwrap();
        for name in [
            "notes.txt",
            "Movie.2019.en.srt",
            "movie.nfo",
            "poster.jpg",
            "sample.mkv",
        ] {
            std::fs::write(movie.join(name), b"not media").unwrap();
        }

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0);
        assert!(
            file_repo
                .find_all_by_library(library.id)
                .await
                .unwrap()
                .is_empty()
        );
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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
    /// touches neither the hasher nor the prober: hashed, and probed (a row
    /// whose probe never succeeded is probed again on every visit).
    fn indexed_file_matching_disk(library_id: Uuid, path: &Path) -> MediaFile {
        let (size_bytes, mtime) = read_fs_meta(path.parent().unwrap(), path).unwrap();
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: path.to_path_buf(),
            hash: 7,
            size_bytes,
            mtime,
            identity: None,
            mime_type: None,
            duration: Some(Duration::from_secs(60)),
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            .scan_library(library.id)
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
            .scan_library(library.id)
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
            .scan_library(library.id)
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

        assert_eq!(service.scan_library(library.id).await.unwrap(), 0);

        // Both rows are legacy: sidecars and stray files are no longer
        // indexed (issue #182), so the one still on disk goes missing too.
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "{files:?}");
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

        assert_eq!(service.scan_library(library.id).await.unwrap(), 0);

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let err = service
            .scan_library(library.id)
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
        // with Unknown status, so added_count is incremented -- and with its
        // real hash, taken before the probe, so the row takes part in
        // duplicate detection rather than carrying the unhashed sentinel.
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
        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(31337));

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
        assert_eq!(
            files[0].hash, 31337,
            "the probe failure keeps the real hash"
        );
        assert_eq!(files[0].classifier_version, 0, "never classified");
    }

    #[tokio::test]
    async fn test_scan_library_real_prober_marks_corrupt_file_unknown() {
        // Same Unknown-on-unprobeable behaviour as above, but exercised through
        // the REAL FFmpeg prober (LocalMediaInfoService) rather than a stub:
        // a corrupt .mp4 makes `from_library_file` return Err, and the scan must
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
            // Every file is hashed before it is probed.
            Arc::new(crate::services::hash::LocalHashService::default()),
            Arc::new(crate::services::media_info::LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
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

        // The hash fails first, so the file is never probed.
        let mock_media_info = MockMediaInfoService::new();

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        // Scan should succeed overall; the failing file is not counted
        let result = service.scan_library(library.id).await;
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

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

        // File A: exists in DB and on disk with the same size and hash →
        // stays unchanged
        let stays_path = dir.path().join("stays.mkv");
        std::fs::write(&stays_path, b"hello").unwrap(); // 5 bytes
        let file_a = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: stays_path.clone(),
            hash: 0,
            size_bytes: 5,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);

        // File B: exists in DB only (phantom, no matching disk file) → will be marked missing
        let phantom_path = dir.path().join("phantom.mkv");
        let file_b = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: phantom_path,
            hash: 0,
            size_bytes: 100,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        // File C: exists on disk only (not in DB) → added as Unknown (its
        // probe fails)
        let new_path = dir.path().join("new_file.mkv");
        std::fs::write(&new_path, b"new").unwrap();
        // File D: a sample → excluded, never indexed
        std::fs::write(dir.path().join("new_file-sample.mkv"), b"sample").unwrap();
        let mut unchanged_hash = MockHashService::new();
        unchanged_hash.expect_hash_async().returning(|_| Ok(0));
        let mut failing_probe = MockMediaInfoService::new();
        failing_probe
            .expect_get_video_metadata()
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".to_string())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(unchanged_hash),
            Arc::new(failing_probe),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let added = service.scan_library(library.id).await.unwrap();
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
        assert_eq!(details["excluded"], serde_json::json!(1));
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
        // As a row records it: at the precision a repository keeps.
        let (size_bytes, mtime) = read_fs_meta(file_path.parent().unwrap(), &file_path).unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 4242,
            size_bytes,
            mtime,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            // Probed: a row whose probe never succeeded is probed again.
            duration: Some(Duration::from_secs(60)),
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

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
            identity: None,
            duration: Some(Duration::from_secs(60)),
            container_format: Some("mp4".to_string()),
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

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
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Changed);
        assert_eq!(files[0].hash, 200);
    }

    /// A file with no movie or episode -- one its path could not classify --
    /// whose content changes and whose re-probe then fails stays `Unknown`:
    /// `Changed` without content is what the `files` CHECK refuses, so the
    /// write would fail and the new hash would never be recorded.
    #[tokio::test]
    async fn test_reconcile_changed_unclassifiable_file_ffmpeg_failure_stays_unknown() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("Show Name/Season 01/Behind the Scenes.mkv");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, b"new content").unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 100,
            size_bytes: 999, // wrong size → suspected
            mtime: None,
            identity: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: Some(Duration::from_secs(60)),
            container_format: Some("matroska".to_string()),
            content: None,
            status: FileStatus::Unknown,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service
            .scan_library(library.id)
            .await
            .expect("the scan does not fail on the file");

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
        assert!(files[0].content.is_none(), "{:?}", files[0].content);
        assert_eq!(files[0].hash, 200, "the new content was recorded");
        assert_eq!(files[0].size_bytes, "new content".len() as u64);
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
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let total = service
            .scan_all_libraries(ScanTrigger::Periodic)
            .await
            .unwrap();
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
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
            identity: None,
            mime_type: None,
            duration: duration_secs.map(Duration::from_secs_f64),
            container_format: None,
            content,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
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
            .find_or_create_by_identity(CreateMovie::new("Some Movie".to_string(), None, None))
            .await
            .unwrap();
        let entry_a = movie_repo
            .find_or_create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: None,
            })
            .await
            .unwrap();
        let entry_b = movie_repo
            .find_or_create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: Some("Extended".to_string()),
            })
            .await
            .unwrap();

        let file_a = make_file_with_content(
            Some(MediaFileContent::movie(entry_a.id)),
            first_secs,
            "/media/movie-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::movie(entry_b.id)),
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

    /// One part of a multi-part movie (issue #233) lasts that part: it is
    /// never compared with a whole file, nor a whole file with it.
    #[tokio::test]
    async fn test_divergence_a_part_is_not_compared_with_a_whole_file() {
        for part_checked in [false, true] {
            // 40 min against 90 min, which diverge as whole files (see
            // `test_divergence_movie_wildly_different_runtimes_warns`).
            let (service, notification, _admin_log_repo, file_a) =
                seed_two_movie_renditions(Some(40.0 * 60.0), Some(90.0 * 60.0)).await;
            let files = &service.file_repo;
            let other = files
                .find_by_path("/media/movie-b.mkv")
                .await
                .unwrap()
                .expect("seeded");
            let (part, whole) = if part_checked {
                (file_a, other)
            } else {
                (other, file_a)
            };
            let Some(MediaFileContent::Movie { movie_entry_id, .. }) = part.content else {
                panic!("seeded a movie file");
            };
            let part = files
                .set_classification(
                    part.id,
                    FileClassification {
                        content: Some(MediaFileContent::Movie {
                            movie_entry_id,
                            part_number: Some(1),
                        }),
                        status: FileStatus::Known,
                        classifier_version: CLASSIFIER_VERSION,
                    },
                )
                .await
                .unwrap();

            let checked = if part_checked { &part } else { &whole };
            service.check_and_report_runtime_divergence(checked).await;

            assert!(
                notification.published_events().is_empty(),
                "part checked: {part_checked}"
            );
        }
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

    /// Both thresholds must be cleared: three hours against three hours five
    /// minutes differ by 300s -- past the 240s floor -- but by under 3%, far
    /// under the 15% ratio, so a long film's cut is not reported.
    #[tokio::test]
    async fn test_divergence_delta_past_the_floor_with_the_ratio_under_no_warning() {
        let (service, notification, admin_log_repo, file_a) =
            seed_two_movie_renditions(Some(180.0 * 60.0), Some(185.0 * 60.0)).await;

        service.check_and_report_runtime_divergence(&file_a).await;

        assert!(
            notification.published_events().is_empty(),
            "a small relative difference must not warn, however many seconds"
        );
        assert!(admin_log_repo.list(100, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_divergence_short_content_guard_no_warning() {
        // Two episodes at 2 min vs 3 min: the ratio (0.33) clears the relative
        // threshold, but the 60s delta is far under the 240s floor → no warning.
        let episode_id = Uuid::new_v4();
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());

        let file_a = make_file_with_content(
            Some(MediaFileContent::episode(episode_id)),
            Some(2.0 * 60.0),
            "/media/ep-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::episode(episode_id)),
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
            Some(MediaFileContent::episode(episode_id)),
            Some(30.0 * 60.0),
            "/media/ep-a.mkv",
        );
        let file_b = make_file_with_content(
            Some(MediaFileContent::episode(episode_id)),
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

    /// A two-episode file runs twice as long as a single episode of the same
    /// first episode; that is not a mismatch, so it is never compared.
    #[tokio::test]
    async fn test_divergence_multi_episode_sibling_is_not_compared_with_a_single_episode() {
        let episode_id = Uuid::new_v4();
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());

        let single = make_file_with_content(
            Some(MediaFileContent::episode(episode_id)),
            Some(30.0 * 60.0),
            "/media/S01E01.mkv",
        );
        let double = make_file_with_content(
            Some(MediaFileContent::Episode {
                episode_id,
                last_episode_number: Some(2),
            }),
            Some(60.0 * 60.0),
            "/media/S01E01E02.mkv",
        );
        let other_double = make_file_with_content(
            Some(MediaFileContent::Episode {
                episode_id,
                last_episode_number: Some(2),
            }),
            Some(20.0 * 60.0),
            "/media/S01E01E02.short.mkv",
        );
        for file in [&single, &double, &other_double] {
            file_repo
                .files
                .lock()
                .unwrap()
                .insert(file.id, (*file).clone());
        }

        let service = make_divergence_service(
            file_repo,
            Arc::new(InMemoryMovieRepository::default()),
            notification.clone(),
            Arc::new(NoOpAdminLogService),
        );

        service.check_and_report_runtime_divergence(&single).await;
        assert!(
            notification.published_events().is_empty(),
            "a single episode is not compared with a two-episode file"
        );

        // Two files of the same range still are.
        service.check_and_report_runtime_divergence(&double).await;
        let warnings = notification.published_events();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("S01E01E02.short.mkv"));
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
            Some(MediaFileContent::episode(Uuid::new_v4())),
            Some(45.0 * 60.0),
            "/media/only.mkv",
        );

        // Must complete (infallible) and publish nothing.
        service.check_and_report_runtime_divergence(&file).await;
        assert!(notification.published_events().is_empty());
    }
}
