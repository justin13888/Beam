use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use serde_json;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::probe::metadata::{StreamMetadata, VideoFileMetadata};
use crate::services::admin_log::AdminLogService;
use crate::services::filesystem_probe::{FilesystemKind, FilesystemProbe, StatfsFilesystemProbe};
use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use crate::services::scan::{
    CANCELLED, CatalogExclusive, ProgressThrottle, SCAN_STOP_TIMEOUT, ScanCoordinator, ScanEvent,
    ScanJob, ScanPhase, ScanProgress, ScanRefused, ScanState, ScanTicket, ScanTrigger, Settle,
    StoppedScan, settle_state,
};
use crate::services::watcher::FsEventKind;
use beam_domain::models::Library;
use beam_domain::models::admin_log::{AdminLogCategory, AdminLogLevel};
use beam_domain::models::enrichment::{EnrichmentTargetId, FieldLocks, MetadataField};
use beam_domain::models::file::{
    CreateMediaFile, FileClassification, FileIdentity, FileRelink, FileStatus, MediaFile,
    MediaFileContent, ProbeUpdate, UpdateMediaFile, displaced_from, mtime_as_stored,
};
use beam_domain::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};
use beam_domain::models::show::{CreateEpisode, CreateShow, Episode, Show};
use beam_domain::models::{PinSource, ProviderPin};
use beam_domain::repositories::{
    AppliedNfoRepository, EnrichmentStateRepository, FileRepository, LibraryRepository,
    MediaStreamRepository, MovieRepository, PlaybackProgressRepository, ShowRepository,
    SidecarSubtitleRepository,
};
use beam_domain::services::{Clock, IdGenerator, RealClock, UuidGenerator};
use beam_domain::utils::classification::{Classification, ContainerTags, Hints, classify};
use beam_domain::utils::filename::{ParsedFilename, parse_media_filename};
use beam_domain::utils::identity::title_identity_key;
use beam_domain::utils::media_path::{
    CLASSIFIER_VERSION, EpisodeInference, MediaInference, MovieInference, TitleGuess,
    UnclassifiableReason, infer_media, season_folder_number,
};
use beam_domain::utils::path_policy::{PathDisposition, PathPolicy, is_video_path};

/// Read the size and modification time of a file in a single stat call,
/// the mtime as [`stored_mtime`] reads it.
fn read_fs_meta(path: &Path) -> std::io::Result<(u64, Option<DateTime<Utc>>)> {
    let meta = std::fs::metadata(path)?;
    Ok((meta.len(), stored_mtime(&meta)))
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

/// Stat `path` for what a row records of it (see [`FileStat`]), on a
/// filesystem whose inodes are `inodes`.
fn read_stat(path: &Path, inodes: Inodes) -> std::io::Result<FileStat> {
    let meta = std::fs::metadata(path)?;
    Ok(FileStat {
        size: meta.len(),
        mtime: stored_mtime(&meta),
        identity: stored_identity(&meta),
        inodes,
    })
}

/// The [`FileIdentity`] `meta` records, its ctime at the precision a row
/// keeps. `None` off Unix: Beam's first-class platforms are Linux and macOS,
/// and elsewhere a file is compared by size and mtime alone.
fn stored_identity(meta: &std::fs::Metadata) -> Option<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime =
            DateTime::from_timestamp(meta.ctime(), meta.ctime_nsec().clamp(0, 999_999_999) as u32)?;
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
fn stored_mtime(meta: &std::fs::Metadata) -> Option<DateTime<Utc>> {
    meta.modified().ok().map(|t| mtime_as_stored(t.into()))
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

/// What a walk of a library root found on disk.
///
/// A named result rather than a bare `Vec` so the walk can report more than
/// the files it reached without changing its call site.
struct WalkOutcome {
    /// Every file under the root the [`PathPolicy`] calls media, in walk
    /// order -- the files the scan indexes.
    files: Vec<PathBuf>,
    /// How many regular files with a video extension the walk saw, *before*
    /// the policy excluded any: the empty-root guard counts these. A root
    /// holding only samples or extras is a mounted root with nothing to
    /// index, not an unmounted one, so the files the policy keeps out still
    /// count here. What the walk never descends into (a hidden or housekeeping
    /// folder, an extras folder, an ignored directory) is not counted.
    video_files_seen: usize,
    /// How many regular files the policy excluded (hidden, extras, samples,
    /// ignore patterns). Reported in the scan's summary.
    excluded: usize,
    /// Every path the walk failed to read: a directory it could not list, or
    /// a listed entry it could not stat for any reason other than "not
    /// found". The walk says nothing about what is beneath one of these, so an
    /// indexed row under it is left exactly as it is rather than marked
    /// missing (issue #179).
    failed_subtrees: Vec<PathBuf>,
    /// Whether the walk hit an error it could not attribute to a path. Nothing
    /// then scopes what the walk failed to see, so no row is marked missing.
    unscoped_failure: bool,
    /// Every text subtitle beside the media (issue #184).
    subtitles: Vec<sidecars::WalkedSidecar>,
    /// Every NFO beside the media (issue #184).
    nfos: Vec<hints::WalkedNfo>,
}

/// Walks a library root and collects every regular file beneath it.
///
/// An entry the walk cannot read is collected as a failure rather than
/// dropped: a subdirectory that fails to list, or a listed file that fails to
/// stat, contributes no files, and
/// reading that silence as "every file under it is gone" is how a transient
/// permission or I/O error used to delete rows.
///
/// Symbolic links under the root are never followed, to a file or to a
/// directory (issue #186): a link is not part of the library. Following one
/// would index files outside the root the administrator registered -- past
/// the containment check library creation makes -- and, for a directory,
/// risk a cycle. The root itself may be a link; only what is beneath it is
/// held to this.
///
/// A directory `policy` excludes wholesale is not descended into (issue
/// #182); a file it does not call media is not collected.
fn walk_library_root(root: &Path, policy: &PathPolicy) -> WalkOutcome {
    walk_under(root, root, policy)
}

/// [`walk_library_root`] from `start`, a directory at or beneath `root`: the
/// walk a watcher event for a directory makes (issue #180). What is beneath
/// `start` is judged by the policy as a full scan would judge it, relative to
/// the library `root`; whether the policy excludes `start` itself is the
/// caller's to ask.
fn walk_under(root: &Path, start: &Path, policy: &PathPolicy) -> WalkOutcome {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut video_files_seen = 0usize;
    let mut excluded = 0usize;
    let mut failed_subtrees: Vec<PathBuf> = Vec::new();
    let mut unscoped_failure = false;
    let mut subtitles: Vec<sidecars::WalkedSidecar> = Vec::new();
    let mut nfos: Vec<hints::WalkedNfo> = Vec::new();
    let walk = WalkDir::new(start)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !(entry.depth() > 0
                && entry.file_type().is_dir()
                && policy.excludes_directory(relative_to(root, entry.path())))
        });
    for entry in walk {
        match entry {
            Ok(entry) => {
                // A directory is descended by the walk itself, which reports
                // any failure to list it as an `Err` below. A symlink is
                // skipped whatever it points at.
                if entry.file_type().is_dir() || entry.file_type().is_symlink() {
                    continue;
                }
                let path = entry.into_path();
                // `symlink_metadata`, so an entry replaced by a link since it
                // was listed is still not followed. Only a stat that says "no
                // such file" (a file deleted mid-walk) means the entry is
                // absent. Any other failure -- a listable but unsearchable
                // parent (EACCES), a transient EIO or ESTALE on a network
                // mount -- says nothing about the file, so it shields the path
                // exactly like a directory the walk could not list (issue
                // #179).
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) => {
                        if !meta.is_file() {
                            continue;
                        }
                        if is_video_path(&path) {
                            video_files_seen += 1;
                        }
                        match policy.disposition(relative_to(root, &path)) {
                            PathDisposition::Media => files.push(path),
                            PathDisposition::Excluded(_) => excluded += 1,
                            PathDisposition::Sidecar => {
                                if sidecars::is_text_subtitle(&path) {
                                    subtitles.push(sidecars::WalkedSidecar {
                                        path,
                                        size: meta.len(),
                                        mtime: stored_mtime(&meta),
                                    });
                                } else if hints::is_nfo(&path) {
                                    nfos.push(hints::WalkedNfo {
                                        stamp: hints::change_stamp(&meta),
                                        path,
                                    });
                                }
                            }
                            PathDisposition::Ignored => {}
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
    WalkOutcome {
        files,
        video_files_seen,
        excluded,
        failed_subtrees,
        unscoped_failure,
        subtitles,
        nfos,
    }
}

/// Whether a library root should be read as unmounted rather than emptied:
/// it holds no video file (`video_files_seen`, as a walk counts them) while
/// video files of it are indexed. An unmounted volume usually leaves its
/// mount point behind as an empty directory -- or one holding only a sentinel
/// or hidden file -- and believing it would mark every row missing. The
/// scan's empty-root guard and the watcher's removed-directory guard ask the
/// same question.
fn root_looks_unmounted(video_files_seen: usize, indexed_video_files: usize) -> bool {
    video_files_seen == 0 && indexed_video_files > 0
}

/// Whether the walk [`walk_library_root`] makes of `root` would see a video
/// file, stopping at the first: all the watcher needs of
/// [`root_looks_unmounted`], without walking a mounted library whole. A walk
/// that fails to read something has not shown the root empty, so it answers
/// that there is one.
fn root_holds_a_video_file(root: &Path, policy: &PathPolicy) -> bool {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !(entry.depth() > 0
                && entry.file_type().is_dir()
                && policy.excludes_directory(relative_to(root, entry.path())))
        })
        .any(|entry| match entry {
            Ok(entry) => entry.file_type().is_file() && is_video_path(entry.path()),
            Err(_) => true,
        })
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

/// Whether nothing is at `path` any more: a stat that does not follow a
/// link says "no such file". Any other failure -- a permission error, a
/// transient I/O error on a network mount -- says nothing about the file, so
/// it is not absent.
fn path_is_absent(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound
    )
}

/// A walked file's size, modification time, identity and content hash, read
/// together once it had settled, on a filesystem whose inodes are `inodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    size: u64,
    mtime: Option<DateTime<Utc>>,
    identity: Option<FileIdentity>,
    inodes: Inodes,
    hash: u64,
}

impl Fingerprint {
    /// The stat this fingerprint was taken at.
    fn stat(&self) -> FileStat {
        FileStat {
            size: self.size,
            mtime: self.mtime,
            identity: self.identity,
            inodes: self.inodes,
        }
    }
}

/// How much `row` looks like the file now at `path`, best first: the same
/// file name, then the same directory, then the row played most recently
/// (by anyone; a row never played last), and last the lowest id, so the
/// choice never depends on the order the rows came in. Breaks a tie
/// between identical copies of a moved file (issue #180).
fn relink_preference(
    path: &Path,
    row: &MediaFile,
    last_played: &HashMap<Uuid, DateTime<Utc>>,
) -> impl Ord + use<> {
    let same_name = row.path.file_name() == path.file_name();
    let same_directory = row.path.parent() == path.parent();
    (
        std::cmp::Reverse(same_name),
        std::cmp::Reverse(same_directory),
        std::cmp::Reverse(last_played.get(&row.id).copied()),
        row.id,
    )
}

/// Which of `candidates` a new file at `path`, of `size` bytes and content
/// hash `hash`, is the same file as -- moved or renamed -- if any (issue
/// #180). The candidates are rows of the file's own library. The watcher's
/// question; a full scan answers it for every path at once, with
/// [`plan_content_moves`].
///
/// A candidate is one with the same content (hash and size) whose own file
/// is gone: marked missing, or no longer at its path per `is_absent`. A row
/// whose file is still there is a copy, not a move, and a new row is right
/// for the new file. A hash of zero is the unhashed sentinel and matches
/// nothing.
///
/// Absent-but-unmarked counts because the watcher delivers the two halves of
/// a move in no particular order: the new path's event can be reconciled
/// before the old path's, while the old row is not yet marked missing.
///
/// Among several, [`relink_preference`] decides.
fn choose_relink_candidate<'a>(
    path: &Path,
    size: u64,
    hash: u64,
    candidates: &'a [MediaFile],
    is_absent: impl Fn(&Path) -> bool,
    last_played: &HashMap<Uuid, DateTime<Utc>>,
) -> Option<&'a MediaFile> {
    if hash == 0 {
        return None;
    }
    candidates
        .iter()
        .filter(|row| row.hash == hash && row.size_bytes == size && row.path != path)
        .filter(|row| row.missing_since.is_some() || is_absent(&row.path))
        .min_by_key(|row| relink_preference(path, row, last_played))
}

/// Whether `row`'s file may no longer be at its path: it is marked missing,
/// or what is there now -- by a stat that does not follow links -- is not
/// what the row recorded. A path that cannot be stat'ed says nothing, so it
/// may have moved too. The watcher leaves a file whose content matches such
/// a row to the next scan rather than guess (issue #180). `inodes` is what
/// the row's library's filesystem keeps.
fn may_have_moved(row: &MediaFile, inodes: Inodes) -> bool {
    row.missing_since.is_some()
        || match std::fs::symlink_metadata(&row.path) {
            Ok(meta) if meta.is_file() => {
                !read_stat(&row.path, inodes).is_ok_and(|stat| stat.is_recorded_by(row))
            }
            _ => true,
        }
}

/// Whether the content `row` records has left its path, as a scan's walk
/// sees it: the path was not walked (gone, or no longer media), or it was
/// and was found holding other content. A row under a path the walk could
/// not read is not known to have moved, and a row never hashed cannot be
/// followed.
fn content_left_its_path(
    row: &MediaFile,
    walked: &std::collections::HashSet<&Path>,
    fingerprints: &HashMap<PathBuf, Fingerprint>,
    is_shielded: &impl Fn(&Path) -> bool,
) -> bool {
    row.hash != 0
        && !is_shielded(&row.path)
        && match fingerprints.get(&row.path) {
            Some(found) => found.hash != row.hash,
            None => !walked.contains(row.path.as_path()),
        }
}

/// A walked path whose content is a row's that has left its own path: the
/// row may be pointed there.
#[derive(Debug, Clone)]
struct ContentMatch<'a> {
    row: &'a MediaFile,
    path: &'a Path,
    found: Fingerprint,
}

/// Every walked path a scan hashed whose content a row records whose own
/// content has left its path (see [`content_left_its_path`]): each pairing
/// [`plan_content_moves`] may choose. A path is a new one, or an indexed one
/// holding content other than its row's; the same content at a row's own
/// path is that row's, and the same content elsewhere while the row's own
/// path still holds it is a copy.
fn content_matches<'a>(
    rows: &'a HashMap<PathBuf, MediaFile>,
    walked: &std::collections::HashSet<&Path>,
    fingerprints: &'a HashMap<PathBuf, Fingerprint>,
    is_shielded: impl Fn(&Path) -> bool,
) -> Vec<ContentMatch<'a>> {
    let mut by_hash: HashMap<u64, Vec<&MediaFile>> = HashMap::new();
    for row in rows.values() {
        if content_left_its_path(row, walked, fingerprints, &is_shielded) {
            by_hash.entry(row.hash).or_default().push(row);
        }
    }
    let mut matches = Vec::new();
    for (path, found) in fingerprints {
        let holds_its_own = rows.get(path).is_some_and(|row| row.hash == found.hash);
        if found.hash == 0 || holds_its_own {
            continue;
        }
        for row in by_hash.get(&found.hash).into_iter().flatten() {
            if row.size_bytes == found.size && row.path != *path {
                matches.push(ContentMatch {
                    row,
                    path,
                    found: *found,
                });
            }
        }
    }
    matches
}

/// The rows a scan points at other paths, and the rows those paths held.
#[derive(Debug, Default)]
struct ContentMoves {
    /// Each row, and the path its content is at now, as found there.
    relinks: Vec<(MediaFile, PathBuf, Fingerprint)>,
    /// Rows at a path a relink takes that are not themselves relinked: their
    /// content is at no path the scan hashed. Kept as missing, aside (see
    /// [`beam_domain::models::displaced_path`]).
    displaced: Vec<MediaFile>,
}

/// Which rows follow their content to another path (issue #180): a move or
/// a rename, two files swapping names, a rotation, a file renamed onto the
/// path of a row already missing. Each row keeps its id, and so its playback
/// progress, its title and its streams.
///
/// Every path gets at most one row and every row at most one path. The
/// pairings are taken best first -- by [`relink_preference`], then by path --
/// so the result depends on neither the order of the walk nor the order of
/// the rows. A row whose path a relink takes, and that is not relinked
/// itself, is displaced.
///
/// Except for a replace-by-rename: a file renamed onto a path whose row
/// someone has played, when nobody has played the renamed file's row. The
/// path keeps its own row, its new content a change to it, and the renamed
/// file's row is left to be marked missing -- so replacing a film with
/// another copy of it keeps everyone's place. Declining such a pairing can
/// free a row or a path for another, so the pairing is planned again
/// without it, until none is left.
///
/// `last_played` holds the rows that are paired and the rows at the paths
/// they are paired with.
fn plan_content_moves(
    rows: &HashMap<PathBuf, MediaFile>,
    matches: Vec<ContentMatch<'_>>,
    last_played: &HashMap<Uuid, DateTime<Utc>>,
) -> ContentMoves {
    let mut matches = matches;
    matches.sort_by(|a, b| {
        relink_preference(a.path, a.row, last_played)
            .cmp(&relink_preference(b.path, b.row, last_played))
            .then_with(|| a.path.cmp(b.path))
    });
    loop {
        let plan = pair_best_first(rows, &matches);
        let displaced: std::collections::HashSet<Uuid> =
            plan.displaced.iter().map(|row| row.id).collect();
        let replaced_by_rename: std::collections::HashSet<(Uuid, PathBuf)> = plan
            .relinks
            .iter()
            .filter(|(row, path, _)| {
                rows.get(path).is_some_and(|holder| {
                    displaced.contains(&holder.id)
                        && last_played.contains_key(&holder.id)
                        && !last_played.contains_key(&row.id)
                })
            })
            .map(|(row, path, _)| (row.id, path.clone()))
            .collect();
        if replaced_by_rename.is_empty() {
            return plan;
        }
        matches.retain(|candidate| {
            !replaced_by_rename.contains(&(candidate.row.id, candidate.path.to_path_buf()))
        });
    }
}

/// [`plan_content_moves`]'s pairing of `matches`, already sorted best first:
/// each is taken unless its row or its path already has been.
fn pair_best_first(
    rows: &HashMap<PathBuf, MediaFile>,
    matches: &[ContentMatch<'_>],
) -> ContentMoves {
    let mut moved: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut taken: std::collections::HashSet<&Path> = std::collections::HashSet::new();
    let mut plan = ContentMoves::default();
    for &ContentMatch { row, path, found } in matches {
        if moved.contains(&row.id) || taken.contains(path) {
            continue;
        }
        moved.insert(row.id);
        taken.insert(path);
        plan.relinks.push((row.clone(), path.to_path_buf(), found));
    }
    plan.displaced = taken
        .iter()
        .filter_map(|path| rows.get(*path))
        .filter(|holder| !moved.contains(&holder.id))
        .cloned()
        .collect();
    plan.displaced.sort_by_key(|row| row.id);
    plan
}

/// Where [`LocalIndexService::process_new_file`] looks for the row a new
/// file may be a moved one of.
enum RelinkSource {
    /// A full scan, whose [`plan_content_moves`] has already relinked every
    /// new file it could.
    Planned,
    /// A watcher event: the library's rows with the file's hash, read from
    /// the repository. Order-independent across debounce windows: the row is
    /// found whether the old path's event was reconciled first or not yet.
    Repository,
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

/// What [`LocalIndexService::backfill_identity_keys`] did: how many legacy
/// titles it keyed, and which it had to leave keyless and why.
#[derive(Debug, Default)]
struct IdentityBackfill {
    keyed: u64,
    /// Titles whose files derive more than one key.
    ambiguous_movies: Vec<Uuid>,
    ambiguous_shows: Vec<Uuid>,
    /// Titles whose key another title already holds.
    clashing_movies: Vec<Uuid>,
    clashing_shows: Vec<Uuid>,
}

/// What [`LocalIndexService::rekey_stale_titles`] did.
#[derive(Debug, Default)]
struct IdentityRekey {
    /// Titles whose key changed in place.
    rekeyed: u64,
    /// `(kept, retired)`: titles merged because the current rules read them
    /// as one.
    merged_movies: Vec<(Uuid, Uuid)>,
    merged_shows: Vec<(Uuid, Uuid)>,
    /// Titles whose files derive more than one key, which keep their old one.
    ambiguous_movies: Vec<Uuid>,
    ambiguous_shows: Vec<Uuid>,
    /// `(stale, holder)`: titles the current rules read as one, left apart
    /// because providers matched them to different entries (see
    /// [`provider_ids_conflict`]). Each keeps its key and its files.
    conflicting_movies: Vec<(Uuid, Uuid)>,
    conflicting_shows: Vec<(Uuid, Uuid)>,
}

/// The one key `rows` derive through `key_of`: from the present files when
/// any derives one, else from every row. `None` when they derive none, or
/// more than one.
fn derived_key(
    rows: &[&(MediaFile, MediaInference)],
    key_of: fn(&MediaInference) -> Option<String>,
) -> Option<String> {
    let keys = |present_only: bool| {
        rows.iter()
            .filter(|(file, _)| !present_only || file.missing_since.is_none())
            .filter_map(|(_, inferred)| key_of(inferred))
            .collect::<std::collections::BTreeSet<String>>()
    };
    let mut keys = match keys(true) {
        present if present.is_empty() => keys(false),
        present => present,
    };
    match keys.len() {
        1 => keys.pop_first(),
        _ => None,
    }
}

/// Whether a provider has matched the title: enrichment, genres and a manual
/// match hang off such a title, so of two titles merged it is the one kept.
fn has_provider_ids(
    tmdb_id: Option<u32>,
    imdb_id: &Option<String>,
    tvdb_id: Option<u32>,
    anilist_id: Option<u32>,
) -> bool {
    tmdb_id.is_some() || imdb_id.is_some() || tvdb_id.is_some() || anilist_id.is_some()
}

/// A title's provider ids, as [`provider_ids_conflict`] compares them.
#[derive(Debug, Clone, Copy)]
struct ProviderIds<'a> {
    tmdb: Option<u32>,
    imdb: Option<&'a str>,
    tvdb: Option<u32>,
    anilist: Option<u32>,
}

impl<'a> ProviderIds<'a> {
    fn of_movie(movie: &'a Movie) -> Self {
        Self {
            tmdb: movie.tmdb_id,
            imdb: movie.imdb_id.as_deref(),
            tvdb: movie.tvdb_id,
            anilist: movie.anilist_id,
        }
    }

    fn of_show(show: &'a Show) -> Self {
        Self {
            tmdb: show.tmdb_id,
            imdb: show.imdb_id.as_deref(),
            tvdb: show.tvdb_id,
            anilist: show.anilist_id,
        }
    }

    fn any(&self) -> bool {
        self.tmdb.is_some() || self.imdb.is_some() || self.tvdb.is_some() || self.anilist.is_some()
    }
}

/// Whether two titles are matched to different provider entries: both carry
/// provider ids, and they disagree on one both carry or share none to agree
/// on. Merging such a pair would discard one match, and matches that
/// disagree say the titles are two, whatever the naming rules read -- `The
/// Godfather Part 2` and `The Godfather` are two films however a rule
/// misreads their names (C3 of the #233 review).
fn provider_ids_conflict(a: ProviderIds<'_>, b: ProviderIds<'_>) -> bool {
    fn agree<T: PartialEq>(a: Option<T>, b: Option<T>) -> Option<bool> {
        Some(a? == b?)
    }
    let verdicts = [
        agree(a.tmdb, b.tmdb),
        agree(a.imdb, b.imdb),
        agree(a.tvdb, b.tvdb),
        agree(a.anilist, b.anilist),
    ];
    a.any() && b.any() && (verdicts.contains(&Some(false)) || !verdicts.contains(&Some(true)))
}

/// Whether title `a` survives a merge with title `b`: the one with provider
/// ids, else the older (`created_at`, then `id`).
fn survives(
    a: (&DateTime<Utc>, &Uuid),
    a_matched: bool,
    b: (&DateTime<Utc>, &Uuid),
    b_matched: bool,
) -> bool {
    match (a_matched, b_matched) {
        (true, false) => true,
        (false, true) => false,
        _ => a <= b,
    }
}

/// The part a movie file on the title keyed `key` is: the one the current
/// rules read from its name, when they key it to that title, else the one
/// stored on it (issue #233).
///
/// The stored part alone is not enough when a rekey settles a title. A file
/// [`LocalIndexService::hold_classification`] held apart carries the current
/// classifier version but never had its part read, and reclassification
/// skips it from then on, so the part it stores -- none -- would stay none
/// once a corrected match merges its title or the title takes its key. A
/// file its name keys elsewhere keeps its stored part until reclassification
/// reads it.
fn part_as_read(file: &MediaFile, inferred: &MediaInference, key: &str) -> Option<u32> {
    match inferred {
        MediaInference::Movie(inferred) if inferred.title.identity_key() == key => {
            inferred.part_number
        }
        _ => match file.content {
            Some(MediaFileContent::Movie { part_number, .. }) => part_number,
            _ => None,
        },
    }
}

/// The identity key of the movie a file's path names, if it names one.
///
/// Used by the identity-key backfill. It is the key classification gives a
/// new title -- `TitleGuess::identity_key` and `CreateMovie::new` /
/// `CreateShow::new` both apply `title_identity_key` to the same inferred
/// title and year -- so a legacy title is keyed from its files' paths exactly
/// as a new one would be.
fn movie_key(inference: &MediaInference) -> Option<String> {
    match inference {
        MediaInference::Movie(movie) => Some(movie.title.identity_key()),
        MediaInference::Episode(_) | MediaInference::Unclassifiable(_) => None,
    }
}

/// The identity key of the show a file's path names, if it is an episode.
fn show_key(inference: &MediaInference) -> Option<String> {
    match inference {
        MediaInference::Episode(episode) => Some(episode.series.identity_key()),
        MediaInference::Movie(_) | MediaInference::Unclassifiable(_) => None,
    }
}

/// Whether a show identity key names a season folder (`season 05|`): the
/// key of a husk, the show a build before issue #182 named after the season
/// folder its episodes sat in. Read from a key, never from the display
/// title, which enrichment rewrites.
fn is_season_folder_husk_key(key: &str) -> bool {
    let title = key.split_once('|').map_or(key, |(title, _)| title);
    season_folder_number(title).is_some()
}

/// Whether `file` was classified by older rules and is to be reclassified:
/// a probed row below [`CLASSIFIER_VERSION`]. An unprobed row -- its probe
/// failed -- was never classified, so there is nothing to reclassify.
fn awaits_reclassification(file: &MediaFile) -> bool {
    let probed = file.content.is_some() || file.duration.is_some();
    file.classifier_version < CLASSIFIER_VERSION && probed
}

/// The key a build before issue #182 derived for the show of the episode at
/// `path`: its parent folder's parse, whatever that folder was.
fn legacy_show_key(path: &Path) -> String {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let ParsedFilename { title, year, .. } = parse_media_filename(&folder);
    title_identity_key(&title, year)
}

/// Records one processed-file outcome on the
/// `beam_index_files_processed_total{result}` counter, covering both full
/// scans and watcher-driven reconciles. `result` is one of `new`, `relinked`,
/// `changed`, `unchanged`, or `failed`. A no-op unless beam-server installed a metrics
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
            IndexError::Db(_) | IndexError::InvalidId | IndexError::ScanInProgress => {
                "internal error".to_string()
            }
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
    progress_repo: Arc<dyn PlaybackProgressRepository>,
    path_policy: PathPolicy,
    enrichment_repo: Option<Arc<dyn EnrichmentStateRepository>>,
    sidecar_repo: Option<Arc<dyn SidecarSubtitleRepository>>,
    applied_nfo_repo: Option<Arc<dyn AppliedNfoRepository>>,
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
        progress_repo: Arc<dyn PlaybackProgressRepository>,
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
            applied_nfo_repo: None,
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

    /// Override the runtime-divergence thresholds used when warning that two
    /// renditions of the same movie/episode disagree on runtime. Defaults to
    /// [`DivergencePolicy::default`].
    pub fn with_divergence_policy(mut self, policy: DivergencePolicy) -> Self {
        self.divergence_policy = policy;
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
    /// name, and stamp the version. The row keeps its id -- and so its
    /// playback progress -- its hash and its probe results; nothing is
    /// re-probed. A title the move leaves with no file (a show a legacy
    /// build named after a `Season 01` folder) is retired by the scan's
    /// orphan cleanup.
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
    async fn process_new_file(
        &self,
        path: &Path,
        library: &Library,
        relink: RelinkSource,
        known: Option<&Fingerprint>,
        inodes: Inodes,
    ) -> Result<FileOutcome, IndexError> {
        info!("Processing new file: {}", path.display());

        let stat = read_stat(path, inodes).map_err(|e| {
            warn!(path = %path.display(), error = %e, "Failed to read file metadata");
            IndexError::PathNotFound(format!("Could not read file metadata: {e}"))
        })?;
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
            _ => self.hash_settled(path, stat).await.map_err(|e| {
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
            if let Some(row) =
                choose_relink_candidate(path, size, hash, &candidates, path_is_absent, &last_played)
            {
                self.relink_files(&[(row.clone(), path.to_path_buf(), found)], &[], library)
                    .await?;
                return Ok(FileOutcome::Relinked);
            }
            // A row whose path now holds something else may be this file,
            // moved in a swap or a rotation whose other halves have not
            // reached the watcher: the scan sees them all.
            if hash != 0 && candidates.iter().any(|row| may_have_moved(row, inodes)) {
                info!(
                    path = %path.display(),
                    "a new file matches a file of the library that may have moved; leaving it to the next scan"
                );
                return Ok(FileOutcome::LeftToScan);
            }
        }

        let metadata = match self.media_info_service.get_video_metadata(path).await {
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
                mime_type: Some(format!("video/{}", metadata.format_name)),
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

    /// Hash `path`, which a stat just before found as `stat`.
    ///
    /// With a settle window, the file is stat'ed again afterwards: `None`
    /// when it changed while it was read, since the hash then describes no
    /// version of the file that ever existed whole.
    async fn hash_settled(&self, path: &Path, stat: FileStat) -> std::io::Result<Option<u64>> {
        let hash = self.hash_service.hash_async(path.to_path_buf()).await?;
        if self.settle_window.is_zero() {
            return Ok(Some(hash));
        }
        let after = read_stat(path, stat.inodes)?;
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

    /// Point each row of `relinks` at the path its file was moved, renamed or
    /// swapped to, as found there, and move each of `displaced` aside as
    /// missing -- in one step, so rows can trade paths (issue #180).
    ///
    /// A relinked row keeps its id, and so its playback progress, its movie
    /// or episode, and its streams; a missing row is visible again. Nothing
    /// is probed or classified: the content is the content the row already
    /// describes. Each move, and each displaced row, is told to the
    /// administrator.
    async fn relink_files(
        &self,
        relinks: &[(MediaFile, PathBuf, Fingerprint)],
        displaced: &[MediaFile],
        library: &Library,
    ) -> Result<(), IndexError> {
        self.file_repo
            .relink(
                relinks
                    .iter()
                    .map(|(row, path, found)| FileRelink {
                        id: row.id,
                        path: path.clone(),
                        size_bytes: found.size,
                        mtime: found.mtime,
                        identity: found.identity,
                    })
                    .collect(),
                displaced.iter().map(|row| row.id).collect(),
                self.clock.now(),
            )
            .await?;
        // A row kept aside is reported by the path it was displaced from:
        // the one it is kept at never existed on disk.
        let moves: Vec<(PathBuf, MediaFile)> = relinks
            .iter()
            .map(|(row, path, _)| {
                let from = displaced_from(&row.path).unwrap_or_else(|| row.path.clone());
                let moved = MediaFile {
                    path: path.clone(),
                    ..row.clone()
                };
                (from, moved)
            })
            .collect();
        // The videos' NFOs' applied state moves with them, all at once, so a
        // swap or a rotation carries each NFO as a move does (FR-219). A
        // failure is the NFOs', not the moves': they are only re-applied
        // later.
        if let Err(e) = self.carry_nfos_on_relink(library, &moves).await {
            warn!(library_id = %library.id, error = %e, "could not carry moved videos' NFO records");
        }
        for ((row, path, _), (from, _)) in relinks.iter().zip(&moves) {
            info!(
                file_id = %row.id,
                from = %from.display(),
                to = %path.display(),
                "a moved file keeps its row"
            );
            record_file_outcome("relinked");
            let message = format!(
                "File moved: '{}' is now '{}'",
                from.display(),
                path.display()
            );
            self.notification_service.publish(AdminEvent::info(
                EventCategory::LibraryScan,
                message.clone(),
                Some(library.id.to_string()),
                Some(library.name.clone()),
            ));
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Info,
                    AdminLogCategory::LibraryScan,
                    message,
                    Some(serde_json::json!({
                        "library_id": library.id.to_string(),
                        "file_id": row.id.to_string(),
                        "from": from.display().to_string(),
                        "to": path.display().to_string(),
                    })),
                )
                .await;
        }
        for row in displaced {
            info!(
                file_id = %row.id,
                path = %row.path.display(),
                "another file's content took a file's path; its row is kept as missing"
            );
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Info,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "File replaced: '{}' now holds another file; its old row is kept as missing",
                        row.path.display()
                    ),
                    Some(serde_json::json!({
                        "library_id": library.id.to_string(),
                        "file_id": row.id.to_string(),
                        "path": row.path.display().to_string(),
                    })),
                )
                .await;
        }
        Ok(())
    }

    /// What is at `path` now, hashed, when it may be content a row does not
    /// already record: a path with no row, or one its row does not record
    /// as it is ([`FileStat::is_recorded_by`]). `None` for a file that is as
    /// recorded, still being written, or cannot be read -- whoever reconciles
    /// it next finds out which, and reports a failure.
    async fn fingerprint(
        &self,
        path: &Path,
        recorded: Option<&MediaFile>,
        inodes: Inodes,
    ) -> Option<Fingerprint> {
        let stat = read_stat(path, inodes).ok()?;
        if recorded.is_some_and(|row| stat.is_recorded_by(row)) {
            return None;
        }
        if let Settle::Unsettled { .. } =
            settle_state(self.clock.now(), stat.mtime, self.settle_window)
        {
            return None;
        }
        let hash = self.hash_settled(path, stat).await.ok()??;
        let FileStat {
            size,
            mtime,
            identity,
            inodes,
        } = stat;
        Some(Fingerprint {
            size,
            mtime,
            identity,
            inodes,
            hash,
        })
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
    /// `inodes` is what the library's filesystem keeps.
    async fn reconcile_existing_file(
        &self,
        existing: &MediaFile,
        path: &Path,
        library: &Library,
        reclassify: bool,
        known: Option<&Fingerprint>,
        inodes: Inodes,
    ) -> Result<FileOutcome, IndexError> {
        if reclassify && awaits_reclassification(existing) {
            self.reclassify_existing(existing, path, library).await?;
        }

        let stat = match read_stat(path, inodes) {
            Ok(stat) => stat,
            Err(e) => {
                // A transient stat failure must not delete or corrupt the row.
                warn!("Failed to stat {}: {}", path.display(), e);
                return Ok(FileOutcome::Unchanged);
            }
        };
        let FileStat {
            size,
            mtime,
            identity,
            inodes: _,
        } = stat;

        let moved = !stat.is_recorded_by(existing);
        // Only a file the path policy calls media -- a video file -- is
        // reconciled at all, so nothing else is ever probed here.
        let unprobed = existing.duration.is_none();

        // Cheap gate: only a size, mtime or identity change warrants a
        // rehash, and only an unprobed file a re-probe.
        if !moved && !unprobed {
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
            match self.hash_settled(path, stat).await {
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
            .reprobe_file(existing, path, library, stat, new_hash)
            .await?;
        if changed {
            record_file_outcome("changed");
            Ok(FileOutcome::Changed)
        } else {
            record_file_outcome("unchanged");
            Ok(FileOutcome::Unchanged)
        }
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
    async fn reprobe_file(
        &self,
        existing: &MediaFile,
        path: &Path,
        library: &Library,
        stat: FileStat,
        new_hash: u64,
    ) -> Result<bool, IndexError> {
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

        match self.media_info_service.get_video_metadata(path).await {
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
                            mime_type: format!("video/{}", metadata.format_name),
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
            // One part of a multi-part movie lasts that part, which no other
            // file of the movie is expected to match.
            Some(MediaFileContent::Movie {
                part_number: Some(_),
                ..
            }) => return,
            Some(MediaFileContent::Movie {
                movie_entry_id,
                part_number: None,
            }) => {
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
                        Ok(files) => collected.extend(files.into_iter().filter(|sibling| {
                            matches!(
                                sibling.content,
                                Some(MediaFileContent::Movie {
                                    part_number: None,
                                    ..
                                })
                            )
                        })),
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
            Some(MediaFileContent::Episode {
                episode_id,
                last_episode_number,
            }) => {
                // A multi-episode file and a single episode share a first
                // episode but not a runtime: compare like with like.
                match self.file_repo.find_by_episode_id(*episode_id).await {
                    Ok(files) => files
                        .into_iter()
                        .filter(|sibling| {
                            matches!(
                                &sibling.content,
                                Some(MediaFileContent::Episode {
                                    last_episode_number: sibling_last,
                                    ..
                                }) if sibling_last == last_episode_number
                            )
                        })
                        .collect(),
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

    /// Give every movie and show that predates identity keys the key its
    /// files' paths derive (issue #183), so the next file of that title finds
    /// it instead of creating a second one.
    ///
    /// The key comes from the paths, not the stored title: enrichment may
    /// already have replaced the title with the provider's spelling, which is
    /// the very thing the key exists to be independent of. Every file row
    /// counts, soft-deleted or not, so a title whose files are all missing at
    /// upgrade -- and may come back -- is keyed as its files say. A title
    /// whose files all derive one key takes it; one with no file row that
    /// parses as its kind (a movie filename for a movie, an episode path for
    /// a show) takes the key of its stored title and year (with no file row
    /// at all, it is about to be deleted as orphaned anyway). A title whose files disagree -- `Dune (1984)` and `Dune
    /// (2021)` once merged under one title -- or whose key another title
    /// already holds -- the duplicate the old title lookup created -- is left
    /// keyless and named in an admin warning: it stays browsable, is never
    /// matched, and new files of it go to the keyed title. A show the rules
    /// before issue #182 named after a season folder -- the name those rules
    /// read from every one of its files' folders is a season folder's -- is
    /// left keyless without a warning: it is a husk
    /// the scan's reclassification empties and orphan cleanup retires.
    async fn backfill_identity_keys(&self) -> Result<IdentityBackfill, IndexError> {
        let mut report = IdentityBackfill::default();

        let movies = self.movie_repo.find_unkeyed().await?;
        let shows = self.show_repo.find_unkeyed().await?;
        if movies.is_empty() && shows.is_empty() {
            return Ok(report);
        }

        // Every file row, soft-deleted ones included: a title whose files are
        // all missing at upgrade still has their paths, and they -- not the
        // display title enrichment may have rewritten -- are what it is keyed
        // by. Read once per library rather than once per title.
        let mut entry_paths: HashMap<Uuid, Vec<MediaInference>> = HashMap::new();
        // Beside each episode file's inference, the key the rules before
        // issue #182 derived from it, which says whether its show is a husk.
        let mut episode_paths: HashMap<Uuid, Vec<(MediaInference, String)>> = HashMap::new();
        for library in self.library_repo.find_all().await? {
            for file in self
                .file_repo
                .find_all_by_library_including_missing(library.id)
                .await?
            {
                let inferred = || infer_media(relative_to(&library.root_path, &file.path));
                match file.content {
                    Some(MediaFileContent::Movie { movie_entry_id, .. }) => entry_paths
                        .entry(movie_entry_id)
                        .or_default()
                        .push(inferred()),
                    Some(MediaFileContent::Episode { episode_id, .. }) => episode_paths
                        .entry(episode_id)
                        .or_default()
                        .push((inferred(), legacy_show_key(&file.path))),
                    None => {}
                }
            }
        }

        // Oldest first (`find_unkeyed`'s order): of two legacy duplicates the
        // original takes the key and the later one is reported.
        for movie in movies {
            let mut keys = std::collections::BTreeSet::new();
            for entry in self.movie_repo.find_entries_by_movie_id(movie.id).await? {
                for inferred in entry_paths.get(&entry.id).into_iter().flatten() {
                    keys.extend(movie_key(inferred));
                }
            }
            let key = match keys.len() {
                0 => title_identity_key(&movie.title, movie.year),
                1 => keys.pop_first().expect("one key"),
                _ => {
                    report.ambiguous_movies.push(movie.id);
                    continue;
                }
            };
            if self
                .movie_repo
                .assign_identity_key(movie.id, &key, CLASSIFIER_VERSION)
                .await?
            {
                report.keyed += 1;
            } else {
                report.clashing_movies.push(movie.id);
            }
        }

        for show in shows {
            let mut keys = std::collections::BTreeSet::new();
            let mut legacy_keys = std::collections::BTreeSet::new();
            for season in self.show_repo.find_seasons_by_show_id(show.id).await? {
                for episode in self.show_repo.find_episodes_by_season_id(season.id).await? {
                    for (inferred, legacy_key) in
                        episode_paths.get(&episode.id).into_iter().flatten()
                    {
                        keys.extend(show_key(inferred));
                        legacy_keys.insert(legacy_key.as_str());
                    }
                }
            }
            // A show named after a season folder (`Season 05`) is the husk a
            // build before issue #182 made by naming shows after the file's
            // parent folder. Keying it with its files' key would hand it the
            // real series' key, and reclassification would then move every
            // file of the series onto the husk. Left keyless, it is never
            // matched: reclassification moves its files to the series they
            // name, and orphan cleanup retires it. It is known by the name
            // those rules gave its files -- every one a season folder's --
            // not by its display title, which enrichment may have replaced.
            // A show with no file row is keyed from its stored title like any
            // fileless title: husk or not, orphan cleanup retires it.
            let husk = !legacy_keys.is_empty()
                && legacy_keys.iter().all(|key| is_season_folder_husk_key(key));
            if husk {
                debug!(show_id = %show.id, title = %show.title, "not keying a season-folder husk");
                continue;
            }
            let key = match keys.len() {
                0 => title_identity_key(&show.title, show.year),
                1 => keys.pop_first().expect("one key"),
                _ => {
                    report.ambiguous_shows.push(show.id);
                    continue;
                }
            };
            if self
                .show_repo
                .assign_identity_key(show.id, &key, CLASSIFIER_VERSION)
                .await?
            {
                report.keyed += 1;
            } else {
                report.clashing_shows.push(show.id);
            }
        }

        if report.keyed > 0 {
            info!("Backfilled identity keys for {} titles", report.keyed);
        }
        let IdentityBackfill {
            keyed: _,
            ambiguous_movies,
            ambiguous_shows,
            clashing_movies,
            clashing_shows,
        } = &report;
        let unkeyed = ambiguous_movies.len()
            + ambiguous_shows.len()
            + clashing_movies.len()
            + clashing_shows.len();
        if unkeyed > 0 {
            warn!(
                unkeyed,
                "titles from before identity keys could not be keyed; they stay listed but new \
                 files will not be matched to them"
            );
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Warning,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "{unkeyed} titles indexed before identity keys could not be keyed: their \
                         files disagree on the title, or another title already has it. They stay \
                         listed, but new files will be matched to other titles."
                    ),
                    Some(serde_json::json!({
                        "ambiguous_movies": ambiguous_movies,
                        "ambiguous_shows": ambiguous_shows,
                        "duplicate_movies": clashing_movies,
                        "duplicate_shows": clashing_shows,
                    })),
                )
                .await;
        }
        Ok(report)
    }

    /// Carry to `survivor` what an administrator set on `loser` (issue #185),
    /// which a merge retires: its pin -- no NFO can bring an administrator's
    /// pin back -- and its field locks, the union of both titles'. Each side
    /// is the title with the pin it holds and who set it. A survivor an
    /// administrator pinned to another id keeps its own pin, and the
    /// administrator is told which was dropped.
    async fn carry_admin_state(
        &self,
        survivor: (EnrichmentTargetId, Option<&str>, Option<PinSource>),
        loser: (EnrichmentTargetId, Option<&str>, Option<PinSource>),
    ) -> Result<(), IndexError> {
        let (survivor, survivor_pin, survivor_source) = survivor;
        let (loser, loser_pin, loser_source) = loser;
        let survivor_admin_pin = survivor_pin.filter(|_| survivor_source == Some(PinSource::Admin));
        let loser_admin_pin = loser_pin
            .filter(|_| loser_source == Some(PinSource::Admin))
            .and_then(ProviderPin::parse);
        let mut rematch = false;
        match (survivor_admin_pin, loser_admin_pin) {
            (_, None) => {}
            (Some(kept), Some(dropped)) => {
                warn!(%kept, %dropped, "merged two titles an administrator pinned to different ids");
                let _ = self
                    .admin_log
                    .log(
                        AdminLogLevel::Warning,
                        AdminLogCategory::Enrichment,
                        format!(
                            "Merged two titles an administrator pinned to different ids: the \
                             pin to {kept} is kept, the pin to {dropped} is dropped"
                        ),
                        Some(serde_json::json!({
                            "kept": survivor.id(),
                            "retired": loser.id(),
                            "kept_pin": kept,
                            "dropped_pin": dropped.to_ref_string(),
                        })),
                    )
                    .await;
            }
            (None, Some(pin)) => {
                // Released first: one id pins one title.
                let carried = match (loser, survivor) {
                    (EnrichmentTargetId::Movie(loser), EnrichmentTargetId::Movie(survivor)) => {
                        self.movie_repo.clear_admin_pin(loser).await?
                            && self
                                .movie_repo
                                .set_pinned_ref(survivor, &pin, PinSource::Admin)
                                .await?
                    }
                    (EnrichmentTargetId::Show(loser), EnrichmentTargetId::Show(survivor)) => {
                        self.show_repo.clear_admin_pin(loser).await?
                            && self
                                .show_repo
                                .set_pinned_ref(survivor, &pin, PinSource::Admin)
                                .await?
                    }
                    _ => false,
                };
                if carried {
                    info!(title = %survivor.id(), %pin, "carried an administrator's pin through a merge");
                    rematch = true;
                } else {
                    warn!(title = %survivor.id(), %pin, "an administrator's pin could not be carried through a merge");
                }
            }
        }

        let Some(states) = &self.enrichment_repo else {
            return Ok(());
        };
        let loser_locks = states
            .find_by_target(loser)
            .await?
            .map(|state| state.locked_fields)
            .unwrap_or_default();
        if !loser_locks.is_empty() {
            let survivor_locks = states
                .find_by_target(survivor)
                .await?
                .map(|state| state.locked_fields)
                .unwrap_or_default();
            let union: FieldLocks = survivor_locks.iter().chain(loser_locks.iter()).collect();
            states.set_locked_fields(survivor, &union).await?;
        }
        if rematch {
            // Fetched by the pin it now carries, not the match it had.
            states.ensure_pending(survivor).await?;
            states.request_refresh(survivor, true).await?;
        }
        Ok(())
    }

    /// Re-derive every identity key an older version of the rules derived
    /// (issue #182), so a title keyed before a change to the title fold or
    /// the path inference is found by the next file that names it rather
    /// than duplicated beside it. `Grey's Anatomy` keyed `grey s anatomy|`
    /// before apostrophes were folded; its next file derives `greys
    /// anatomy|`.
    ///
    /// Each title's new key is its present files' key, derived exactly as
    /// the backfill derives one; with no present file, every file row's
    /// (a volume away at upgrade still names its titles). A title whose
    /// files name no title of its kind keeps its key and version, and is
    /// looked at again on the next process start; one whose files disagree
    /// keeps its key and is named in an admin warning.
    ///
    /// A free key is taken in place: the title keeps its id, enrichment,
    /// genres, provider ids and manual match. A key another title holds means
    /// the two are one title: the one with provider ids survives (else the
    /// older), takes the key, and receives the other's files -- its entries,
    /// or its seasons and episodes, found or created on the survivor -- and
    /// the other, left with no file, is retired by the scan's orphan cleanup.
    /// A show keyed after a season folder (`season 05|`) is released instead,
    /// never rekeyed, for the reason the backfill leaves one keyless.
    async fn rekey_stale_titles(&self) -> Result<IdentityRekey, IndexError> {
        let mut report = IdentityRekey::default();

        let movies = self
            .movie_repo
            .find_keyed_before_version(CLASSIFIER_VERSION)
            .await?;
        let shows = self
            .show_repo
            .find_keyed_before_version(CLASSIFIER_VERSION)
            .await?;
        if movies.is_empty() && shows.is_empty() {
            return Ok(report);
        }

        // Every file row, soft-deleted ones included, with what its path
        // names; read once, and kept current as merges move files.
        let mut entry_files: HashMap<Uuid, Vec<(MediaFile, MediaInference)>> = HashMap::new();
        let mut episode_files: HashMap<Uuid, Vec<(MediaFile, MediaInference)>> = HashMap::new();
        for library in self.library_repo.find_all().await? {
            for file in self
                .file_repo
                .find_all_by_library_including_missing(library.id)
                .await?
            {
                let inferred = infer_media(relative_to(&library.root_path, &file.path));
                match file.content {
                    Some(MediaFileContent::Movie { movie_entry_id, .. }) => entry_files
                        .entry(movie_entry_id)
                        .or_default()
                        .push((file, inferred)),
                    Some(MediaFileContent::Episode { episode_id, .. }) => episode_files
                        .entry(episode_id)
                        .or_default()
                        .push((file, inferred)),
                    None => {}
                }
            }
        }

        // Titles already settled by this pass -- rekeyed, released, or a
        // merge's survivor or loser -- which a later stale row must not undo.
        let mut settled: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

        for stale in movies {
            if settled.contains(&stale.id) {
                continue;
            }
            // Re-read: an earlier merge may have changed or released the key.
            let Some(movie) = self.movie_repo.find_by_id(stale.id).await? else {
                continue;
            };
            if movie.identity_key != stale.identity_key {
                continue;
            }
            let mut rows: Vec<&(MediaFile, MediaInference)> = Vec::new();
            for entry in self.movie_repo.find_entries_by_movie_id(movie.id).await? {
                rows.extend(entry_files.get(&entry.id).into_iter().flatten());
            }
            let Some(key) = derived_key(&rows, movie_key) else {
                if rows
                    .iter()
                    .any(|(_, inferred)| movie_key(inferred).is_some())
                {
                    report.ambiguous_movies.push(movie.id);
                }
                continue;
            };
            // The title the current rules spell from these files.
            let spelled: Option<TitleGuess> =
                rows.iter().find_map(|(_, inferred)| match inferred {
                    MediaInference::Movie(movie) if movie.title.identity_key() == key => {
                        Some(movie.title.clone())
                    }
                    _ => None,
                });
            let holder = match self.movie_repo.find_by_identity_key(&key).await? {
                Some(holder) if holder.id != movie.id => holder,
                _ => {
                    if self
                        .movie_repo
                        .rekey(movie.id, Some(key.clone()), CLASSIFIER_VERSION)
                        .await?
                    {
                        if movie.identity_key.as_deref() != Some(key.as_str()) {
                            report.rekeyed += 1;
                        }
                        settled.insert(movie.id);
                        self.retitle_as_spelled(&movie, &key, spelled.as_ref())
                            .await?;
                        self.read_held_parts(&rows, &key).await?;
                    }
                    continue;
                }
            };
            if provider_ids_conflict(
                ProviderIds::of_movie(&movie),
                ProviderIds::of_movie(&holder),
            ) {
                warn!(stale = %movie.id, holder = %holder.id, %key, "two movies matched to different provider entries read as one; kept apart");
                self.hold_classification(&rows).await?;
                report.conflicting_movies.push((movie.id, holder.id));
                settled.insert(movie.id);
                continue;
            }
            let (survivor, loser) = if survives(
                (&movie.created_at, &movie.id),
                has_provider_ids(
                    movie.tmdb_id,
                    &movie.imdb_id,
                    movie.tvdb_id,
                    movie.anilist_id,
                ),
                (&holder.created_at, &holder.id),
                has_provider_ids(
                    holder.tmdb_id,
                    &holder.imdb_id,
                    holder.tvdb_id,
                    holder.anilist_id,
                ),
            ) {
                (movie.id, holder.id)
            } else {
                (holder.id, movie.id)
            };
            // Release first: the survivor may be taking the loser's key.
            self.movie_repo
                .rekey(loser, None, CLASSIFIER_VERSION)
                .await?;
            if !self
                .movie_repo
                .rekey(survivor, Some(key.clone()), CLASSIFIER_VERSION)
                .await?
            {
                // Another writer took the key in between: the survivor is
                // still on its stale key, so moving files onto it would
                // strand them there. Both keep their files; the loser,
                // released, is never matched again.
                warn!(%survivor, %loser, %key, "a movie merge lost its key to another writer");
                settled.insert(survivor);
                settled.insert(loser);
                continue;
            }
            if survivor == movie.id {
                // Its files stay where they are; the holder's move below.
                self.read_held_parts(&rows, &key).await?;
            }
            for entry in self.movie_repo.find_entries_by_movie_id(loser).await? {
                let MovieEntry {
                    id: entry_id,
                    library_id,
                    movie_id: _,
                    edition,
                    created_at: _,
                } = entry;
                let target = self
                    .movie_repo
                    .find_or_create_entry(CreateMovieEntry {
                        library_id,
                        movie_id: survivor,
                        edition,
                    })
                    .await?;
                self.movie_repo
                    .ensure_library_association(library_id, survivor)
                    .await?;
                let moved = entry_files.remove(&entry_id).unwrap_or_default();
                for (file, inferred) in &moved {
                    let part_number = part_as_read(file, inferred, &key);
                    self.move_file(
                        file,
                        MediaFileContent::Movie {
                            movie_entry_id: target.id,
                            part_number,
                        },
                    )
                    .await?;
                }
                entry_files.entry(target.id).or_default().extend(moved);
            }
            let (kept, retired) = if survivor == movie.id {
                (&movie, &holder)
            } else {
                (&holder, &movie)
            };
            self.retitle_as_spelled(kept, &key, spelled.as_ref())
                .await?;
            self.carry_admin_state(
                (
                    EnrichmentTargetId::Movie(survivor),
                    kept.pinned_ref.as_deref(),
                    kept.pin_source,
                ),
                (
                    EnrichmentTargetId::Movie(loser),
                    retired.pinned_ref.as_deref(),
                    retired.pin_source,
                ),
            )
            .await?;
            report.merged_movies.push((survivor, loser));
            settled.insert(survivor);
            settled.insert(loser);
        }

        for stale in shows {
            if settled.contains(&stale.id) {
                continue;
            }
            let Some(show) = self.show_repo.find_by_id(stale.id).await? else {
                continue;
            };
            if show.identity_key != stale.identity_key {
                continue;
            }
            // A husk (see `backfill_identity_keys`): released, so the scan's
            // reclassification moves its files to the series they name and
            // orphan cleanup retires it. Known by its stored key, which
            // enrichment never rewrites, not by its display title.
            if show
                .identity_key
                .as_deref()
                .is_some_and(is_season_folder_husk_key)
            {
                self.show_repo
                    .rekey(show.id, None, CLASSIFIER_VERSION)
                    .await?;
                settled.insert(show.id);
                continue;
            }
            let mut rows: Vec<&(MediaFile, MediaInference)> = Vec::new();
            for season in self.show_repo.find_seasons_by_show_id(show.id).await? {
                for episode in self.show_repo.find_episodes_by_season_id(season.id).await? {
                    rows.extend(episode_files.get(&episode.id).into_iter().flatten());
                }
            }
            let Some(key) = derived_key(&rows, show_key) else {
                if rows
                    .iter()
                    .any(|(_, inferred)| show_key(inferred).is_some())
                {
                    report.ambiguous_shows.push(show.id);
                }
                continue;
            };
            let holder = match self.show_repo.find_by_identity_key(&key).await? {
                Some(holder) if holder.id != show.id => holder,
                _ => {
                    if self
                        .show_repo
                        .rekey(show.id, Some(key.clone()), CLASSIFIER_VERSION)
                        .await?
                    {
                        if show.identity_key.as_deref() != Some(key.as_str()) {
                            report.rekeyed += 1;
                        }
                        settled.insert(show.id);
                    }
                    continue;
                }
            };
            if provider_ids_conflict(ProviderIds::of_show(&show), ProviderIds::of_show(&holder)) {
                warn!(stale = %show.id, holder = %holder.id, %key, "two shows matched to different provider entries read as one; kept apart");
                self.hold_classification(&rows).await?;
                report.conflicting_shows.push((show.id, holder.id));
                settled.insert(show.id);
                continue;
            }
            let (survivor, loser) = if survives(
                (&show.created_at, &show.id),
                has_provider_ids(show.tmdb_id, &show.imdb_id, show.tvdb_id, show.anilist_id),
                (&holder.created_at, &holder.id),
                has_provider_ids(
                    holder.tmdb_id,
                    &holder.imdb_id,
                    holder.tvdb_id,
                    holder.anilist_id,
                ),
            ) {
                (show.id, holder.id)
            } else {
                (holder.id, show.id)
            };
            self.show_repo
                .rekey(loser, None, CLASSIFIER_VERSION)
                .await?;
            if !self
                .show_repo
                .rekey(survivor, Some(key.clone()), CLASSIFIER_VERSION)
                .await?
            {
                // As for a movie merge: files are never moved onto a
                // survivor left on its stale key.
                warn!(%survivor, %loser, %key, "a show merge lost its key to another writer");
                settled.insert(survivor);
                settled.insert(loser);
                continue;
            }
            for season in self.show_repo.find_seasons_by_show_id(loser).await? {
                let target_season = self
                    .show_repo
                    .find_or_create_season(survivor, season.season_number)
                    .await?;
                for episode in self.show_repo.find_episodes_by_season_id(season.id).await? {
                    let Episode {
                        id: episode_id,
                        season_id: _,
                        episode_number,
                        title,
                        description: _,
                        air_date,
                        runtime,
                        thumbnail_url: _,
                        created_at: _,
                    } = episode;
                    // Found or created on the survivor: one episode per
                    // `(season, number)`, so a number both shows have
                    // becomes one episode with both shows' files.
                    let target = self
                        .show_repo
                        .find_or_create_episode(CreateEpisode {
                            season_id: target_season.id,
                            episode_number,
                            title,
                            runtime,
                            air_date: air_date.as_deref().and_then(|d| {
                                chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
                            }),
                        })
                        .await?;
                    let moved = episode_files.remove(&episode_id).unwrap_or_default();
                    for (file, _) in &moved {
                        let last_episode_number = match file.content {
                            Some(MediaFileContent::Episode {
                                last_episode_number,
                                ..
                            }) => last_episode_number,
                            _ => None,
                        };
                        self.show_repo
                            .ensure_library_association(file.library_id, survivor)
                            .await?;
                        self.move_file(
                            file,
                            MediaFileContent::Episode {
                                episode_id: target.id,
                                last_episode_number,
                            },
                        )
                        .await?;
                    }
                    episode_files.entry(target.id).or_default().extend(moved);
                }
            }
            let (kept, retired) = if survivor == show.id {
                (&show, &holder)
            } else {
                (&holder, &show)
            };
            self.carry_admin_state(
                (
                    EnrichmentTargetId::Show(survivor),
                    kept.pinned_ref.as_deref(),
                    kept.pin_source,
                ),
                (
                    EnrichmentTargetId::Show(loser),
                    retired.pinned_ref.as_deref(),
                    retired.pin_source,
                ),
            )
            .await?;
            report.merged_shows.push((survivor, loser));
            settled.insert(survivor);
            settled.insert(loser);
        }

        let IdentityRekey {
            rekeyed,
            merged_movies,
            merged_shows,
            ambiguous_movies,
            ambiguous_shows,
            conflicting_movies,
            conflicting_shows,
        } = &report;
        if *rekeyed > 0 || !merged_movies.is_empty() || !merged_shows.is_empty() {
            let merged = merged_movies.len() + merged_shows.len();
            info!(rekeyed, merged, "Re-derived identity keys from older rules");
            let pairs = |pairs: &[(Uuid, Uuid)]| {
                pairs
                    .iter()
                    .map(|(survivor, retired)| {
                        serde_json::json!({ "kept": survivor, "retired": retired })
                    })
                    .collect::<Vec<_>>()
            };
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Info,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "Updated the identity keys of {rekeyed} titles to the current naming \
                         rules, and merged {merged} titles those rules now read as one"
                    ),
                    Some(serde_json::json!({
                        "rekeyed": rekeyed,
                        "merged_movies": pairs(merged_movies),
                        "merged_shows": pairs(merged_shows),
                    })),
                )
                .await;
        }
        let ambiguous = ambiguous_movies.len() + ambiguous_shows.len();
        if ambiguous > 0 {
            warn!(
                ambiguous,
                "titles whose files name more than one title keep their old identity keys"
            );
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Warning,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "{ambiguous} titles keep identity keys from older naming rules: their \
                         files name more than one title, so new files may be matched to other \
                         titles."
                    ),
                    Some(serde_json::json!({
                        "ambiguous_movies": ambiguous_movies,
                        "ambiguous_shows": ambiguous_shows,
                    })),
                )
                .await;
        }
        let conflicting = conflicting_movies.len() + conflicting_shows.len();
        if conflicting > 0 {
            let pairs = |pairs: &[(Uuid, Uuid)]| {
                pairs
                    .iter()
                    .map(|(stale, holder)| serde_json::json!({ "stale": stale, "holder": holder }))
                    .collect::<Vec<_>>()
            };
            let _ = self
                .admin_log
                .log(
                    AdminLogLevel::Warning,
                    AdminLogCategory::LibraryScan,
                    format!(
                        "{conflicting} pairs of titles the current naming rules read as one are \
                         matched to different provider entries, so they are kept apart: each \
                         keeps its identity key and its files. Rename the files or correct a \
                         match to settle them; a pair whose matches agree is merged the next \
                         time the server starts."
                    ),
                    Some(serde_json::json!({
                        "conflicting_movies": pairs(conflicting_movies),
                        "conflicting_shows": pairs(conflicting_shows),
                    })),
                )
                .await;
        }
        Ok(report)
    }

    /// Stamp the files `rows` names with the current rules' version, as
    /// classified: a title kept apart from the one its files now key to
    /// keeps them, where reclassification would move them to that title --
    /// the merge [`Self::rekey_stale_titles`] declined.
    async fn hold_classification(
        &self,
        rows: &[&(MediaFile, MediaInference)],
    ) -> Result<(), DbErr> {
        for (file, _) in rows {
            if file.classifier_version >= CLASSIFIER_VERSION {
                continue;
            }
            self.file_repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: file.content.clone(),
                        status: file.status,
                        classifier_version: CLASSIFIER_VERSION,
                    },
                )
                .await?;
        }
        Ok(())
    }

    /// Give each movie file in `rows` -- the files of a title just settled on
    /// `key`, rekeyed in place or kept by a merge -- the part the current
    /// rules read from its name, where that is not the part it has. A file
    /// [`Self::hold_classification`] held needs it: it carries the current
    /// version, so reclassification never reads its part. A file still
    /// awaiting reclassification gets early the part reclassification would
    /// give it; one classified by the current rules already has it.
    async fn read_held_parts(
        &self,
        rows: &[&(MediaFile, MediaInference)],
        key: &str,
    ) -> Result<(), DbErr> {
        for (file, inferred) in rows {
            let Some(MediaFileContent::Movie {
                movie_entry_id,
                part_number,
            }) = file.content
            else {
                continue;
            };
            let read = part_as_read(file, inferred, key);
            if read == part_number {
                continue;
            }
            self.move_file(
                file,
                MediaFileContent::Movie {
                    movie_entry_id,
                    part_number: read,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Give `movie` -- as read before its key was re-derived as `key` -- the
    /// display title `spelled`, the one the current rules read from its
    /// files, when the rules changed its key and the title it has is still
    /// the one older rules read from them: its display title keys to the key
    /// those rules gave it. `Movie - CD1` becomes `Movie` once part tokens are
    /// read (issue #233). A title whose key the rules left alone keeps its
    /// spelling; a title a provider matched, an NFO named or an administrator
    /// locked is kept, and so is one changed since `movie` was read.
    async fn retitle_as_spelled(
        &self,
        movie: &Movie,
        key: &str,
        spelled: Option<&TitleGuess>,
    ) -> Result<(), IndexError> {
        let Some(spelled) = spelled else {
            return Ok(());
        };
        if movie.identity_key.as_deref() == Some(key) {
            return Ok(());
        }
        let matched = has_provider_ids(
            movie.tmdb_id,
            &movie.imdb_id,
            movie.tvdb_id,
            movie.anilist_id,
        );
        let as_old_rules_spelled = movie.identity_key.as_deref()
            == Some(title_identity_key(&movie.title, movie.year).as_str());
        let same_year = spelled.year == movie.year;
        if matched || !as_old_rules_spelled || !same_year || spelled.title == movie.title {
            return Ok(());
        }
        if let Some(states) = &self.enrichment_repo
            && states
                .find_by_target(EnrichmentTargetId::Movie(movie.id))
                .await?
                .is_some_and(|state| state.locked_fields.is_locked(MetadataField::Title))
        {
            return Ok(());
        }
        if self
            .movie_repo
            .retitle_from(movie.id, &movie.title, &spelled.title)
            .await?
        {
            info!(movie_id = %movie.id, from = %movie.title, to = %spelled.title, "retitled a movie to the current naming rules");
        }
        Ok(())
    }

    /// Point `file` at `content`, keeping its status and classifier version.
    async fn move_file(&self, file: &MediaFile, content: MediaFileContent) -> Result<(), DbErr> {
        self.file_repo
            .set_classification(
                file.id,
                FileClassification {
                    content: Some(content),
                    status: file.status,
                    classifier_version: file.classifier_version,
                },
            )
            .await
            .map(|_| ())
    }

    /// Whether the identity passes have succeeded in this process, running
    /// them first if they have not: the backfill of titles that predate
    /// identity keys, then the re-derivation of keys older rules derived.
    ///
    /// Every path that reclassifies a file -- a scan of every library, a scan
    /// of one, a watcher event for a file [awaiting
    /// reclassification](awaits_reclassification) -- asks this first and reclassifies only on
    /// `true`, so a file is reclassified only once its title carries the key
    /// the current rules derive, and finds that title rather than leaving it
    /// with no file to be retired with its enrichment. A failed pass is
    /// logged, reported to the administrator, and retried by the next caller;
    /// until one succeeds, files classified by older rules keep their titles
    /// while new and changed files are indexed as usual.
    ///
    /// The passes run holding the catalog gate exclusively (issue #181), so
    /// this waits for every running scan and reconcile to let go of it, and
    /// none classifies a file -- or finds or creates a title by its key --
    /// until the passes are done. Never call it while holding a
    /// [`ScanGuard`](crate::services::scan::ScanGuard).
    async fn identity_passes_done(&self) -> bool {
        if self.identity_passes_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let exclusive = self.scans.exclusive_catalog().await;
        self.run_identity_passes(exclusive).await
    }

    /// [`Self::identity_passes_done`] for a watcher event, which never waits:
    /// `false` when a scan or reconcile holds the catalog gate. The file then
    /// keeps its classification until a later visit.
    async fn identity_passes_done_now(&self) -> bool {
        if self.identity_passes_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let Some(exclusive) = self.scans.try_exclusive_catalog() else {
            return false;
        };
        self.run_identity_passes(exclusive).await
    }

    /// Run the identity passes under `_exclusive`, unless another caller ran
    /// them to success while this one waited for the gate.
    async fn run_identity_passes(&self, _exclusive: CatalogExclusive) -> bool {
        if self.identity_passes_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let failure = match self.backfill_identity_keys().await {
            Ok(_) => match self.rekey_stale_titles().await {
                Ok(_) => None,
                Err(e) => Some(("re-derivation", e)),
            },
            Err(e) => Some(("backfill", e)),
        };
        match failure {
            None => {
                self.identity_passes_succeeded
                    .store(true, Ordering::Release);
                true
            }
            Some((pass, e)) => {
                error!(pass, error = %e, "identity key pass failed; reclassification held");
                let _ = self
                    .admin_log
                    .log(
                        AdminLogLevel::Warning,
                        AdminLogCategory::LibraryScan,
                        format!(
                            "Titles could not be brought up to the current naming rules ({e}). \
                             Files indexed under older rules keep their titles until a later \
                             scan succeeds; new files are indexed as usual."
                        ),
                        Some(serde_json::json!({ "pass": pass, "error": e.to_string() })),
                    )
                    .await;
                false
            }
        }
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
        // shields the same path (issue #179). The stat does not follow a
        // symlink: a link is not a library file (issue #186), so a path that
        // is now one reconciles exactly like a path that is gone.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "could not stat a changed path; leaving its row as it is"
                );
                return Ok(ReconcileOutcome::Done);
            }
        };

        // A subtitle or an NFO beside the media (issue #184). Handled here,
        // before the file-row bookkeeping below -- which still marks missing a
        // row a build before issue #182 made of one.
        if self
            .path_policy
            .disposition(relative_to(&library.root_path, &path))
            == PathDisposition::Sidecar
        {
            let is_file = meta.as_ref().is_some_and(std::fs::Metadata::is_file);
            if hints::is_nfo(&path) {
                self.reconcile_nfo_event(&library, &path, is_file, &[])
                    .await?;
            } else {
                self.reconcile_sidecar_event(&library, &path, is_file)
                    .await?;
            }
        }

        let outcome = match meta {
            Some(meta) if meta.is_file() => {
                let inodes = self.inodes_of(&library);
                self.reconcile_file(&path, &library, reclassify, inodes)
                    .await?
            }
            Some(meta) if meta.is_dir() => {
                let inodes = self.inodes_of(&library);
                return self.reconcile_directory(&path, &library, inodes).await;
            }
            _ => {
                self.reconcile_gone(&path, &library).await?;
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
        // -- is marked missing, exactly as a full scan would leave it.
        if self
            .path_policy
            .disposition(relative_to(&library.root_path, path))
            != PathDisposition::Media
        {
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
                let found = self.fingerprint(path, Some(&existing), inodes).await;
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
                                && may_have_moved(row, inodes)
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
                    inodes,
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
        // reconciled first, has already moved the rows along.
        let gone: Vec<MediaFile> = self
            .file_repo
            .find_beneath_including_missing(library.id, path)
            .await?
            .into_iter()
            .filter(|row| row.missing_since.is_none() && path_is_absent(&row.path))
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
            Some(library.id.to_string()),
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
        let library_id = Some(library.id.to_string());
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
    pub(crate) async fn scan_library(&self, library_id: String) -> Result<u32, IndexError> {
        let library_id = Uuid::parse_str(&library_id).map_err(|_| IndexError::InvalidId)?;
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
    /// hash taken here is the one reconciling it reuses. `inodes` is what the
    /// library's filesystem keeps.
    async fn fingerprint_walk(
        &self,
        walked_files: &[PathBuf],
        rows: &HashMap<PathBuf, MediaFile>,
        walked: &std::collections::HashSet<&Path>,
        is_shielded: &impl Fn(&Path) -> bool,
        ticket: &ScanTicket,
        inodes: Inodes,
    ) -> Result<HashMap<PathBuf, Fingerprint>, IndexError> {
        let mut changed = 0usize;
        let mut to_hash: Vec<&PathBuf> = Vec::new();
        for path in walked_files {
            let row = rows.get(path);
            let Ok(stat) = read_stat(path, inodes) else {
                continue;
            };
            match row {
                Some(row) if stat.is_recorded_by(row) => continue,
                Some(_) => changed += 1,
                None => {}
            }
            to_hash.push(path);
        }
        let gone = rows.values().any(|row| {
            row.hash != 0 && !walked.contains(row.path.as_path()) && !is_shielded(&row.path)
        });
        let mut fingerprints = HashMap::new();
        if !(gone || changed > 0) {
            return Ok(fingerprints);
        }
        for path in to_hash {
            if ticket.is_cancelled() {
                info!("Scan cancelled");
                return Err(IndexError::Cancelled);
            }
            if let Some(found) = self.fingerprint(path, rows.get(path), inodes).await {
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
        } = walk_library_root(&library.root_path, &self.path_policy);
        let walked_media = walked_files.clone();

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
        let inodes = self.inodes_of(library);
        let fingerprints = self
            .fingerprint_walk(
                &walked_files,
                &existing_map,
                &walked,
                &is_shielded,
                ticket,
                inodes,
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
                            inodes,
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
            Some(lib_uuid.to_string()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
        let indexed = service.scan_library(library.id.to_string()).await.unwrap();

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
        let indexed = service.scan_library(library.id.to_string()).await.unwrap();

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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

        let result = service.scan_library(library.id.to_string()).await;
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

        let result = service.scan_library(library.id.to_string()).await;
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
    /// touches neither the hasher nor the prober: hashed, and probed (a row
    /// whose probe never succeeded is probed again on every visit).
    fn indexed_file_matching_disk(library_id: Uuid, path: &Path) -> MediaFile {
        let (size_bytes, mtime) = read_fs_meta(path).unwrap();
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

        let result = service.scan_library(library.id.to_string()).await;
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
            // Every file is hashed before it is probed.
            Arc::new(crate::services::hash::LocalHashService::default()),
            Arc::new(crate::services::media_info::LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

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
        let (size_bytes, mtime) = read_fs_meta(&file_path).unwrap();

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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

        service.scan_library(library.id.to_string()).await.unwrap();

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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        );

        service
            .scan_library(library.id.to_string())
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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
