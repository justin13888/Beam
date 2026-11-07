//! The walk of a library root: which paths a scan visits, what each one's
//! stat says, and whether the root looks unmounted.

use super::*;

/// What a walk of a library root found on disk.
///
/// A named result rather than a bare `Vec` so the walk can report more than
/// the files it reached without changing its call site.
#[derive(Default)]
pub(super) struct WalkOutcome {
    /// Every file under the root the [`PathPolicy`] calls media, in walk
    /// order, then the stream files each disc structure's main title plays
    /// -- the files the scan indexes.
    pub(super) files: Vec<PathBuf>,
    /// How many regular files with a video extension the walk saw, *before*
    /// the policy excluded any: the empty-root guard counts these. A root
    /// holding only samples or extras is a mounted root with nothing to
    /// index, not an unmounted one, so the files the policy keeps out still
    /// count here. What the walk never descends into (a hidden or housekeeping
    /// folder, an extras folder, an ignored directory) is not counted. A disc
    /// structure's stream files count, whether its main title plays them or
    /// not (issue #234): a library of disc rips is a mounted one.
    pub(super) video_files_seen: usize,
    /// How many regular files the policy excluded (hidden, extras, samples,
    /// ignore patterns). Reported in the scan's summary.
    pub(super) excluded: usize,
    /// Every path the walk failed to read: a directory it could not list, or
    /// a listed entry it could not stat for any reason other than "not
    /// found". The walk says nothing about what is beneath one of these, so an
    /// indexed row under it is left exactly as it is rather than marked
    /// missing (issue #179).
    pub(super) failed_subtrees: Vec<PathBuf>,
    /// Whether the walk hit an error it could not attribute to a path. Nothing
    /// then scopes what the walk failed to see, so no row is marked missing.
    pub(super) unscoped_failure: bool,
    /// Every text subtitle beside the media (issue #184).
    pub(super) subtitles: Vec<sidecars::WalkedSidecar>,
    /// Every NFO beside the media (issue #184).
    pub(super) nfos: Vec<hints::WalkedNfo>,
    /// The root of every disc structure the walk read (issue #234), whether
    /// or not it could be read whole: the discs whose files' parts
    /// [`LocalIndexService::rederive_disc_parts`] brings up to date.
    pub(super) discs: Vec<PathBuf>,
}

impl WalkOutcome {
    /// Read the disc structure of `kind` rooted at `disc_root`, beneath the
    /// library `root`, for its main title ([`disc::read_disc`]): its stream
    /// files are seen, those its main title plays collected, and the rest
    /// excluded. A disc read only in part is a path the walk failed to read:
    /// a title chosen from part of a disc may not be its main title, so its
    /// rows are left as they are.
    pub(super) fn add_disc(
        &mut self,
        root: &Path,
        disc_root: &Path,
        kind: DiscKind,
        policy: &PathPolicy,
    ) {
        let DiscRead {
            title,
            streams,
            failed,
        } = disc::read_disc(root, disc_root, kind, policy);
        self.video_files_seen += streams;
        self.excluded += streams.saturating_sub(title.len());
        if failed {
            self.failed_subtrees.push(disc_root.to_path_buf());
        }
        self.files.extend(title);
        self.discs.push(disc_root.to_path_buf());
    }
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
pub(super) fn walk_library_root(root: &Path, policy: &PathPolicy) -> WalkOutcome {
    walk_under(root, root, policy)
}

/// [`walk_library_root`] from `start`, a directory at or beneath `root`: the
/// walk a watcher event for a directory makes (issue #180). What is beneath
/// `start` is judged by the policy as a full scan would judge it, relative to
/// the library `root`; whether the policy excludes `start` itself is the
/// caller's to ask.
///
/// A DVD or Blu-ray disc structure is not walked as files: its folder is read
/// whole for its main title ([`disc::read_disc`], issue #234), and only the
/// stream files that title plays are collected. A walk that starts at or
/// inside a disc reads that disc.
pub(super) fn walk_under(root: &Path, start: &Path, policy: &PathPolicy) -> WalkOutcome {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut video_files_seen = 0usize;
    let mut excluded = 0usize;
    let mut failed_subtrees: Vec<PathBuf> = Vec::new();
    let mut unscoped_failure = false;
    let mut subtitles: Vec<sidecars::WalkedSidecar> = Vec::new();
    let mut nfos: Vec<hints::WalkedNfo> = Vec::new();
    if let Some((disc_root, kind)) = policy.disc_root(relative_to(root, start)) {
        let mut outcome = WalkOutcome::default();
        outcome.add_disc(root, &root.join(disc_root), kind, policy);
        return outcome;
    }
    let mut discs: Vec<(PathBuf, DiscKind)> = Vec::new();
    let mut cursor = StatCursor::new(root);
    let mut walk = WalkDir::new(start).follow_links(false).into_iter();
    while let Some(entry) = walk.next() {
        match entry {
            Ok(entry) => {
                // A directory is descended by the walk itself, which reports
                // any failure to list it as an `Err` below -- unless the
                // policy excludes it, or it is a disc, read whole below. A
                // symlink is skipped whatever it points at.
                if entry.file_type().is_dir() {
                    if entry.depth() > 0 {
                        let rel = relative_to(root, entry.path());
                        if policy.excludes_directory(rel) {
                            walk.skip_current_dir();
                        } else if let Some((_, kind)) = policy.disc_root(rel) {
                            walk.skip_current_dir();
                            discs.push((entry.into_path(), kind));
                        }
                    }
                    continue;
                }
                if entry.file_type().is_symlink() {
                    continue;
                }
                let path = entry.into_path();
                // Stat'ed beneath the root with no link followed (issue
                // #238), so an entry replaced by a link since it was listed,
                // or one whose folder has been, is not followed, and one that
                // is no regular file is skipped: neither is part of the
                // library. Only a stat that says "no such file" (a file
                // deleted mid-walk) means the entry is absent. Any other
                // failure -- a listable but unsearchable parent (EACCES), a
                // transient EIO or ESTALE on a network mount -- says nothing
                // about the file, so it shields the path exactly like a
                // directory the walk could not list (issue #179).
                match cursor.stat(&path) {
                    Ok(meta) => {
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
                                        size: meta.size(),
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
                            // Never met here: a disc's root folder is read
                            // whole rather than descended.
                            PathDisposition::DiscStream(_) => excluded += 1,
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) if is_refusal(&err) => {
                        debug!(path = %path.display(), error = %err, "library walk skipped an entry that is no regular file reached without a link");
                    }
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
    let mut outcome = WalkOutcome {
        files,
        video_files_seen,
        excluded,
        failed_subtrees,
        unscoped_failure,
        subtitles,
        nfos,
        discs: Vec::new(),
    };
    for (disc_root, kind) in discs {
        outcome.add_disc(root, &disc_root, kind, policy);
    }
    outcome
}

/// The walked `files` of the library rooted at `root`, each stat'ed as a row
/// records it, beneath the root with no link followed and without opening
/// it ([`StatCursor`]), on a filesystem whose inodes are `inodes`.
///
/// A file that is no longer a regular file reached without a link -- it, or
/// a folder above it, has become a link since the walk listed it -- is
/// dropped (issue #238): it is no file of the library, exactly as a link the
/// walk saw is not, so its row is treated as missing. A file whose stat fails
/// for any other reason is kept, unstat'ed: that failure says nothing about
/// what is there. Stops with [`IndexError::Cancelled`] once `ticket` is
/// cancelled.
pub(super) fn stat_walked(
    root: &Path,
    files: Vec<PathBuf>,
    inodes: Inodes,
    ticket: &ScanTicket,
) -> Result<(Vec<PathBuf>, HashMap<PathBuf, FileStat>), IndexError> {
    let mut library_stats = LibraryStats::new(root, inodes);
    let mut kept = Vec::with_capacity(files.len());
    let mut stats = HashMap::new();
    for path in files {
        if ticket.is_cancelled() {
            info!("Scan cancelled");
            return Err(IndexError::Cancelled);
        }
        match library_stats.stat(&path) {
            Ok(stat) => {
                stats.insert(path.clone(), stat);
                kept.push(path);
            }
            Err(err) if is_refusal(&err) => {
                info!(
                    path = %path.display(),
                    error = %err,
                    "a walked file is no longer a regular file beneath the root reached without a link; treating it as missing"
                );
            }
            Err(_) => kept.push(path),
        }
    }
    Ok((kept, stats))
}

/// Whether a library root should be read as unmounted rather than emptied:
/// it holds no video file (`video_files_seen`, as a walk counts them) while
/// video files of it are indexed. An unmounted volume usually leaves its
/// mount point behind as an empty directory -- or one holding only a sentinel
/// or hidden file -- and believing it would mark every row missing. The
/// scan's empty-root guard and the watcher's removed-directory guard ask the
/// same question.
pub(super) fn root_looks_unmounted(video_files_seen: usize, indexed_video_files: usize) -> bool {
    video_files_seen == 0 && indexed_video_files > 0
}

/// Whether the walk [`walk_library_root`] makes of `root` would see a video
/// file, stopping at the first: all the watcher needs of
/// [`root_looks_unmounted`], without walking a mounted library whole. A walk
/// that fails to read something has not shown the root empty, so it answers
/// that there is one.
pub(super) fn root_holds_a_video_file(root: &Path, policy: &PathPolicy) -> bool {
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
