//! Subtitle files beside a video, indexed as subtitles of that video (issue
//! #184), and the NFOs a scan or the watcher finds changed.
//!
//! Nothing here writes to a library root: a subtitle is only ever stat-ed and
//! its directory listed. What a subtitle's name says is read by
//! [`beam_domain::utils::sidecar`]; this module decides which indexed video
//! owns it and keeps the `sidecar_subtitles` rows in step with the disk.
//! Nothing references a sidecar row, so one whose file is gone -- or that no
//! indexed video owns any more -- is deleted outright.

use std::collections::{HashMap, HashSet};

use beam_domain::models::applied_nfo::AppliedNfo;
use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat, UpsertSidecarSubtitle};
use beam_domain::utils::sidecar::{is_subtitle_folder, match_sidecar};

use super::*;

/// A file the walk found beside the media, with what its stat said.
#[derive(Debug, Clone)]
pub(super) struct WalkedSidecar {
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) mtime: Option<DateTime<Utc>>,
}

/// Whether `path` names a text subtitle Beam indexes.
pub(super) fn is_text_subtitle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(SubtitleFormat::from_extension)
        .is_some()
}

/// Whether the folder at `dir` is a `Subs/` or `Subtitles/` folder.
fn in_subtitle_folder(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_subtitle_folder)
}

/// The folders whose videos may own a subtitle in `dir`: `dir`, and the
/// folder above when `dir` is a `Subs/` folder.
fn owner_folders(dir: &Path) -> Vec<&Path> {
    let mut folders = vec![dir];
    if in_subtitle_folder(dir)
        && let Some(above) = dir.parent()
    {
        folders.push(above);
    }
    folders
}

/// Whether a walk that failed at `failed_subtrees` (or somewhere it could not
/// say, `unscoped_failure`) can vouch that nothing is at `path`.
fn walk_saw(path: &Path, failed_subtrees: &[PathBuf], unscoped_failure: bool) -> bool {
    !unscoped_failure
        && !failed_subtrees
            .iter()
            .any(|failed| path.starts_with(failed))
}

impl LocalIndexService {
    /// The regular files in `dir` the path policy calls media. Listing a
    /// folder reads nothing inside it and follows no link.
    fn videos_in(&self, library: &Library, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .map(|entry| entry.path())
            .filter(|path| {
                self.path_policy
                    .disposition(relative_to(&library.root_path, path))
                    == PathDisposition::Media
            })
            .collect()
    }

    /// Bring the library's sidecar rows in line with what a scan's walk
    /// found: every subtitle an indexed video in `media` owns is recorded --
    /// written only when new or changed -- and every row no longer found, or
    /// no longer owned, is deleted, unless the walk failed where it lives.
    pub(super) async fn reconcile_sidecars(
        &self,
        library: &Library,
        media: &[PathBuf],
        subtitles: &[WalkedSidecar],
        failed_subtrees: &[PathBuf],
        unscoped_failure: bool,
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.sidecar_repo else {
            return Ok(());
        };
        let indexed: HashMap<PathBuf, Uuid> = self
            .file_repo
            .find_all_by_library(library.id)
            .await?
            .into_iter()
            .map(|file| (file.path, file.id))
            .collect();
        let mut by_folder: HashMap<&Path, Vec<&Path>> = HashMap::new();
        for video in media.iter().filter(|path| indexed.contains_key(*path)) {
            if let Some(dir) = video.parent() {
                by_folder.entry(dir).or_default().push(video);
            }
        }

        let mut found: HashMap<&Path, UpsertSidecarSubtitle> = HashMap::new();
        for subtitle in subtitles {
            let Some(dir) = subtitle.path.parent() else {
                continue;
            };
            let candidates: Vec<&Path> = owner_folders(dir)
                .into_iter()
                .flat_map(|folder| by_folder.get(folder).into_iter().flatten().copied())
                .collect();
            let Some((video, info)) = match_sidecar(&subtitle.path, &candidates) else {
                continue;
            };
            found.insert(
                &subtitle.path,
                UpsertSidecarSubtitle {
                    file_id: indexed[video],
                    library_id: library.id,
                    path: subtitle.path.clone(),
                    info,
                    size_bytes: subtitle.size,
                    mtime: subtitle.mtime,
                },
            );
        }

        let stored = repo.find_all_by_library(library.id).await?;
        let stored_by_path: HashMap<&Path, &SidecarSubtitle> =
            stored.iter().map(|row| (row.path.as_path(), row)).collect();
        let mut written = 0usize;
        for (path, upsert) in &found {
            let unchanged = stored_by_path
                .get(path)
                .is_some_and(|row| upsert.matches(row));
            if !unchanged {
                repo.upsert_by_path(upsert.clone()).await?;
                written += 1;
            }
        }
        let gone: Vec<Uuid> = stored
            .iter()
            .filter(|row| !found.contains_key(row.path.as_path()))
            .filter(|row| walk_saw(&row.path, failed_subtrees, unscoped_failure))
            .map(|row| row.id)
            .collect();
        let deleted = repo.delete_by_ids(gone).await?;
        if written > 0 || deleted > 0 {
            info!(
                library_id = %library.id,
                written,
                deleted,
                "Reconciled sidecar subtitles"
            );
        }
        Ok(())
    }

    /// Reconcile the subtitle at `path` after a watcher event: record it
    /// against the indexed video that owns it, or delete its row when it is
    /// gone or no indexed video owns it. A subtitle whose video is not
    /// indexed yet is recorded when that video is (see
    /// [`Self::attach_adjacent_sidecars`]).
    pub(super) async fn reconcile_sidecar_event(
        &self,
        library: &Library,
        path: &Path,
        is_file: bool,
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.sidecar_repo else {
            return Ok(());
        };
        if !is_text_subtitle(path) {
            return Ok(());
        }
        let stored = repo.find_by_path(path).await?;
        if !is_file {
            // A root that is not there is a volume that went away, not a
            // subtitle that was deleted (issue #179).
            if library.root_path.is_dir()
                && let Some(row) = stored
            {
                repo.delete_by_ids(vec![row.id]).await?;
            }
            return Ok(());
        }
        let Ok((size, mtime)) = read_fs_meta(&library.root_path, path) else {
            // A failed stat says nothing about the file: leave its row.
            return Ok(());
        };
        let Some(dir) = path.parent() else {
            return Ok(());
        };
        let videos: Vec<PathBuf> = owner_folders(dir)
            .into_iter()
            .flat_map(|folder| self.videos_in(library, folder))
            .collect();
        let candidates: Vec<&Path> = videos.iter().map(PathBuf::as_path).collect();
        let owner = match match_sidecar(path, &candidates) {
            Some((video, info)) => self
                .file_repo
                .find_by_path(&video.to_string_lossy())
                .await?
                .filter(|file| file.missing_since.is_none())
                .map(|file| (file.id, info)),
            None => None,
        };
        match owner {
            Some((file_id, info)) => {
                let upsert = UpsertSidecarSubtitle {
                    file_id,
                    library_id: library.id,
                    path: path.to_path_buf(),
                    info,
                    size_bytes: size,
                    mtime,
                };
                if !stored.as_ref().is_some_and(|row| upsert.matches(row)) {
                    repo.upsert_by_path(upsert).await?;
                }
            }
            None => {
                if let Some(row) = stored {
                    repo.delete_by_ids(vec![row.id]).await?;
                }
            }
        }
        Ok(())
    }

    /// The text subtitles beside `video` -- in its folder, and in a `Subs/`
    /// or `Subtitles/` folder beside it -- that the path policy calls
    /// sidecars: every subtitle that video could own.
    fn adjacent_subtitles(&self, library: &Library, video: &Path) -> Vec<PathBuf> {
        let Some(dir) = video.parent() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut subtitles = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_file() && is_text_subtitle(&path) {
                subtitles.push(path);
            } else if file_type.is_dir() && in_subtitle_folder(&path) {
                let Ok(inner) = std::fs::read_dir(&path) else {
                    continue;
                };
                subtitles.extend(
                    inner
                        .flatten()
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                        .map(|e| e.path())
                        .filter(|p| is_text_subtitle(p)),
                );
            }
        }
        subtitles.retain(|subtitle| {
            self.path_policy
                .disposition(relative_to(&library.root_path, subtitle))
                == PathDisposition::Sidecar
        });
        subtitles
    }

    /// Record the subtitles beside a video the watcher just indexed: those in
    /// its folder, and in a `Subs/` or `Subtitles/` folder beside it.
    pub(super) async fn attach_adjacent_sidecars(
        &self,
        library: &Library,
        video: &Path,
    ) -> Result<(), IndexError> {
        if self.sidecar_repo.is_none() {
            return Ok(());
        }
        for subtitle in self.adjacent_subtitles(library, video) {
            self.reconcile_sidecar_event(library, &subtitle, true)
                .await?;
        }
        Ok(())
    }

    /// Bring the subtitles of a video the watcher just relinked to `video`
    /// (issue #180) in line with its new path: a subtitle is owned by path,
    /// so each of its rows for a subtitle not beside `video` is reconciled
    /// again -- deleted when the subtitle is gone or no video owns it, moved
    /// to the video beside it that does -- and the subtitles beside `video`
    /// are recorded against it.
    pub(super) async fn relink_sidecars(
        &self,
        library: &Library,
        video: &Path,
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.sidecar_repo else {
            return Ok(());
        };
        let Some(file) = self
            .file_repo
            .find_by_path(&video.to_string_lossy())
            .await?
        else {
            return Ok(());
        };
        let adjacent = self.adjacent_subtitles(library, video);
        for row in repo.find_by_file_id(file.id).await? {
            if adjacent.contains(&row.path) {
                continue;
            }
            // Stat'ed with no link followed beneath the root (issue #238), as
            // a watcher event's path is: one reached through a folder that
            // has become a link is no file of the library, and its record is
            // dropped.
            let is_file = stat_regular_file(&library.root_path, &row.path).is_ok();
            self.reconcile_sidecar_event(library, &row.path, is_file)
                .await?;
        }
        for subtitle in adjacent {
            self.reconcile_sidecar_event(library, &subtitle, true)
                .await?;
        }
        Ok(())
    }

    /// Re-apply the NFOs a scan's walk found whose content changed since they
    /// were last applied (FR-219) -- whatever their modification times say,
    /// so an NFO copied in with an old mtime (`cp -p`, `rsync -a`), one on a
    /// NAS whose clock disagrees with the server's, and one a previous scan
    /// died before reaching are all applied. An NFO whose stat stamp matches
    /// its record was not written since and is not read. Classification has
    /// already recorded the NFOs it read for new files, so those are not
    /// applied a second time. The record of an NFO the walk no longer finds is
    /// deleted, unless the walk failed where it lives.
    ///
    /// Without an [`AppliedNfoRepository`] there is no record to compare
    /// against, so nothing is re-applied here.
    pub(super) async fn reapply_changed_nfos(
        &self,
        library: &Library,
        nfos: &[hints::WalkedNfo],
        failed_subtrees: &[PathBuf],
        unscoped_failure: bool,
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.applied_nfo_repo else {
            return Ok(());
        };
        let stored: HashMap<PathBuf, AppliedNfo> = repo
            .find_all_by_library(library.id)
            .await?
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect();

        // The library's files are read once, and only if some NFO changed,
        // then indexed by folder and by the folder above that.
        let mut files: Option<Vec<MediaFile>> = None;
        let mut by_parent: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        let mut by_grandparent: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for walked in nfos {
            let record = stored.get(&walked.path);
            if let (Some(stamp), Some(record)) = (&walked.stamp, record)
                && record.change_stamp.as_ref() == Some(stamp)
            {
                continue;
            }
            if files.is_none() {
                let all = self.file_repo.find_all_by_library(library.id).await?;
                for (i, file) in all.iter().enumerate() {
                    let parent = file.path.parent();
                    if let Some(parent) = parent {
                        by_parent.entry(parent.to_path_buf()).or_default().push(i);
                    }
                    if let Some(grandparent) = parent.and_then(Path::parent) {
                        by_grandparent
                            .entry(grandparent.to_path_buf())
                            .or_default()
                            .push(i);
                    }
                }
                files = Some(all);
            }
            let all = files.as_deref().unwrap_or_default();
            let Some(dir) = walked.path.parent() else {
                continue;
            };
            let mut indices: Vec<usize> = by_parent.get(dir).cloned().unwrap_or_default();
            if hints::is_tvshow_nfo(&walked.path) {
                indices.extend(by_grandparent.get(dir).into_iter().flatten().copied());
            }
            let candidates: Vec<&MediaFile> = indices
                .into_iter()
                .map(|i| &all[i])
                .filter(|file| hints::may_describe(&walked.path, &file.path))
                .collect();
            self.reapply_nfo(library, &walked.path, record, &candidates)
                .await?;
        }

        let walked: HashSet<&Path> = nfos.iter().map(|nfo| nfo.path.as_path()).collect();
        let gone: Vec<Uuid> = stored
            .values()
            .filter(|record| !walked.contains(record.path.as_path()))
            .filter(|record| walk_saw(&record.path, failed_subtrees, unscoped_failure))
            .map(|record| record.id)
            .collect();
        repo.delete_by_ids(gone).await?;
        Ok(())
    }

    /// Reconcile the NFO at `path` after a watcher event: re-apply it when
    /// its content changed since it was last applied, to the indexed files
    /// it can describe -- read by one query for its folder, never the whole
    /// library's files -- or forget its record when it is gone.
    ///
    /// A changed NFO the watcher cannot yet tell moved from edited is left
    /// as it is -- neither applied nor recorded -- for the next scan, which
    /// sees every path at once and carries a moved NFO with its video
    /// (FR-219): one beside a video this event left to the scan
    /// (`left_to_scan`, one half of a swap or a rotation) or whose file is
    /// no longer the one its row records, and one holding what another NFO
    /// path's record holds, where the file there no longer does -- moved
    /// from there, perhaps ahead of its video's event. Without an
    /// [`AppliedNfoRepository`] the scan re-applies nothing, so nothing is
    /// left to it.
    pub(super) async fn reconcile_nfo_event(
        &self,
        library: &Library,
        path: &Path,
        is_file: bool,
        left_to_scan: &[PathBuf],
    ) -> Result<(), IndexError> {
        let stored = match &self.applied_nfo_repo {
            Some(repo) => repo.find_by_path(path).await?,
            None => None,
        };
        if !is_file {
            // A root that is not there is a volume that went away, not an NFO
            // that was deleted (issue #179).
            if library.root_path.is_dir()
                && let (Some(repo), Some(record)) = (&self.applied_nfo_repo, stored)
            {
                repo.delete_by_ids(vec![record.id]).await?;
            }
            return Ok(());
        }
        let Some(dir) = path.parent() else {
            return Ok(());
        };
        let Some(read) = hints::read_nfo_file(&library.root_path, path) else {
            return Ok(());
        };
        let under = self.file_repo.find_all_under(library.id, dir).await?;
        let candidates: Vec<&MediaFile> = under
            .iter()
            .filter(|file| hints::may_describe(path, &file.path))
            .collect();
        if !stored
            .as_ref()
            .is_some_and(|stored| read.content.same_as(stored))
            && self
                .nfo_may_have_moved(library, path, &read.content, &candidates, left_to_scan)
                .await?
        {
            info!(
                path = %path.display(),
                "a changed NFO may have moved with a video; leaving it to the next scan"
            );
            return Ok(());
        }
        self.reapply_read_nfo(library, path, stored.as_ref(), &candidates, read)
            .await
    }

    /// Whether the NFO at `path`, found holding `content` that its record
    /// does not, may be one half of a move the watcher cannot sort out: it
    /// may describe a video of `left_to_scan`, or one of the indexed
    /// `candidates` whose file is no longer the one its row records (moved
    /// or swapped, its own event not yet reconciled or left to the scan);
    /// or another NFO path's record holds `content` and the file there no
    /// longer does -- gone, or holding other content. The library's NFO
    /// records are read only here, for an NFO that changed.
    async fn nfo_may_have_moved(
        &self,
        library: &Library,
        path: &Path,
        content: &hints::NfoContent,
        candidates: &[&MediaFile],
        left_to_scan: &[PathBuf],
    ) -> Result<bool, IndexError> {
        let Some(repo) = &self.applied_nfo_repo else {
            return Ok(false);
        };
        if left_to_scan
            .iter()
            .any(|video| hints::may_describe(path, video))
            || {
                let inodes = self.inodes_of(library);
                candidates
                    .iter()
                    .any(|file| may_have_moved(&library.root_path, file, inodes))
            }
        {
            return Ok(true);
        }
        Ok(repo
            .find_all_by_library(library.id)
            .await?
            .iter()
            .any(|record| {
                record.path != path
                    && content.same_as(record)
                    && (path_is_absent(&record.path)
                        || hints::read_nfo_file(&library.root_path, &record.path)
                            .is_some_and(|read| !read.content.same_as(record)))
            }))
    }
}
