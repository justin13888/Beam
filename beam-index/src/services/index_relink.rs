//! Relinking: pointing an indexed row at the path its file was moved,
//! renamed or swapped to, found by inode or by content (issue #180).

use super::*;

/// What to do with the indexed rows a scan's walk did not see.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct MissingPlan {
    /// Rows to stamp `missing_since = now`: not yet marked, and not under a
    /// path the walk failed to read.
    ///
    /// With a zero grace period a row can be in both `mark` and `purge`.
    /// [`FileRepository::purge_missing`] only removes rows that are already
    /// stamped, so the scan marks before it purges.
    pub(super) mark: Vec<Uuid>,
    /// Rows missing for at least the grace period, to purge.
    pub(super) purge: Vec<Uuid>,
    /// How many unseen rows were left untouched because the walk could not
    /// vouch for their absence.
    pub(super) shielded: usize,
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
pub(super) fn plan_missing<'a>(
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

/// Whether no file or folder of the library rooted at `root` is at `path`
/// any more: a stat that follows no link beneath the root
/// ([`StatCursor::kind`], issue #246) says "no such file", finds something
/// that is neither -- a link, a FIFO -- or is refused because a folder above
/// the path is a link now ([`is_refusal`]). Any other failure -- a
/// permission error, a transient I/O error on a network mount -- says
/// nothing about the file, so it is not absent.
pub(super) fn path_is_absent(root: &Path, path: &Path) -> bool {
    match StatCursor::new(root).kind(path) {
        Ok(EntryKind::File | EntryKind::Folder) => false,
        Ok(EntryKind::Other) => true,
        Err(err) => err.kind() == std::io::ErrorKind::NotFound || is_refusal(&err),
    }
}

/// A walked file's size, modification time, identity and content hash, read
/// together once it had settled, on a filesystem whose inodes are `inodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fingerprint {
    pub(super) size: u64,
    pub(super) mtime: Option<DateTime<Utc>>,
    pub(super) identity: Option<FileIdentity>,
    pub(super) inodes: Inodes,
    pub(super) hash: u64,
}

impl Fingerprint {
    /// The stat this fingerprint was taken at.
    pub(super) fn stat(&self) -> FileStat {
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
pub(super) fn relink_preference(
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
pub(super) fn choose_relink_candidate<'a>(
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
/// or what is there now -- by a stat that follows no link beneath the
/// library `root` ([`read_stat`]) -- is not what the row recorded. A path
/// that cannot be stat'ed says nothing, so it may have moved too. The
/// watcher leaves a file whose content matches such a row to the next scan
/// rather than guess (issue #180). `inodes` is what the row's library's
/// filesystem keeps.
pub(super) fn may_have_moved(root: &Path, row: &MediaFile, inodes: Inodes) -> bool {
    row.missing_since.is_some()
        || !read_stat(root, &row.path, inodes).is_ok_and(|stat| stat.is_recorded_by(row))
}

/// Whether the content `row` records has left its path, as a scan's walk
/// sees it: the path was not walked (gone, or no longer media), or it was
/// and was found holding other content. A row under a path the walk could
/// not read is not known to have moved, and a row never hashed cannot be
/// followed.
pub(super) fn content_left_its_path(
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
pub(super) struct ContentMatch<'a> {
    pub(super) row: &'a MediaFile,
    pub(super) path: &'a Path,
    pub(super) found: Fingerprint,
}

/// Every walked path a scan hashed whose content a row records whose own
/// content has left its path (see [`content_left_its_path`]): each pairing
/// [`plan_content_moves`] may choose. A path is a new one, or an indexed one
/// holding content other than its row's; the same content at a row's own
/// path is that row's, and the same content elsewhere while the row's own
/// path still holds it is a copy.
pub(super) fn content_matches<'a>(
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
pub(super) struct ContentMoves {
    /// Each row, and the path its content is at now, as found there.
    pub(super) relinks: Vec<(MediaFile, PathBuf, Fingerprint)>,
    /// Rows at a path a relink takes that are not themselves relinked: their
    /// content is at no path the scan hashed. Kept as missing, aside (see
    /// [`beam_domain::models::displaced_path`]).
    pub(super) displaced: Vec<MediaFile>,
}

/// Which rows follow their content to another path (issue #180): a move or
/// a rename, two files swapping names, a rotation, a file renamed onto the
/// path of a row already missing. Each row keeps its id, and so its title,
/// its streams, and its place as the file a viewer's watch state last played.
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
pub(super) fn plan_content_moves(
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
pub(super) fn pair_best_first(
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
pub(super) enum RelinkSource {
    /// A full scan, whose [`plan_content_moves`] has already relinked every
    /// new file it could.
    Planned,
    /// A watcher event: the library's rows with the file's hash, read from
    /// the repository. Order-independent across debounce windows: the row is
    /// found whether the old path's event was reconciled first or not yet.
    Repository,
}

impl LocalIndexService {
    /// Point each row of `relinks` at the path its file was moved, renamed or
    /// swapped to, as found there, and move each of `displaced` aside as
    /// missing -- in one step, so rows can trade paths (issue #180).
    ///
    /// A relinked row keeps its id, and so its movie or episode, its streams,
    /// and its place as the file a viewer's watch state last played; a
    /// missing row is visible again. Nothing is probed or classified: the
    /// content is the content the row already describes. Each move, and each
    /// displaced row, is told to the administrator.
    pub(super) async fn relink_files(
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
                Some(library.id),
                Some(library.name.clone()),
            ));
            let _ = self
                .admin_log
                .create(CreateAdminLog {
                    level: AdminLogLevel::Info,
                    category: AdminLogCategory::LibraryScan,
                    message,
                    details: Some(serde_json::json!({
                        "library_id": library.id.to_string(),
                        "file_id": row.id.to_string(),
                        "from": from.display().to_string(),
                        "to": path.display().to_string(),
                    })),
                })
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
                .create(CreateAdminLog {
                    level: AdminLogLevel::Info,
                    category: AdminLogCategory::LibraryScan,
                    message: format!(
                        "File replaced: '{}' now holds another file; its old row is kept as missing",
                        row.path.display()
                    ),
                    details: Some(serde_json::json!({
                        "library_id": library.id.to_string(),
                        "file_id": row.id.to_string(),
                        "path": row.path.display().to_string(),
                    })),
                })
                .await;
        }
        Ok(())
    }

    /// What is at `path` now, hashed, when it may be content a row does not
    /// already record: a path with no row, or one its row does not record
    /// as it is ([`FileStat::is_recorded_by`]). `Ok(None)` for a file that is
    /// as recorded, still being written, or cannot be hashed -- whoever
    /// reconciles it next finds out which, and reports a failure. The file
    /// is stat'ed beneath the library `root` with no link followed, and
    /// opened -- the same way -- only to be hashed, its stat then read from
    /// the handle it is hashed from. An `Err` is a failure to stat or open
    /// it, which [`is_refusal`] may say means no file of the library is there.
    pub(super) async fn fingerprint(
        &self,
        root: &Path,
        path: &Path,
        recorded: Option<&MediaFile>,
        inodes: Inodes,
    ) -> std::io::Result<Option<Fingerprint>> {
        let listed = read_stat(root, path, inodes)?;
        if recorded.is_some_and(|row| listed.is_recorded_by(row)) {
            return Ok(None);
        }
        let opened = LibraryFile::open(root, path)?;
        let stat = FileStat::of(&FileMeta::from(opened.metadata()), inodes);
        if let Settle::Unsettled { .. } =
            settle_state(self.clock.now(), stat.mtime, self.settle_window)
        {
            return Ok(None);
        }
        let Ok(Some(hash)) = self.hash_settled(root, &opened, stat).await else {
            return Ok(None);
        };
        let FileStat {
            size,
            mtime,
            identity,
            inodes,
        } = stat;
        Ok(Some(Fingerprint {
            size,
            mtime,
            identity,
            inodes,
            hash,
        }))
    }
}
