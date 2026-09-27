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
        let Ok((size, mtime)) = read_fs_meta(path) else {
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
        let Some(dir) = video.parent() else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
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
        for subtitle in subtitles {
            if self
                .path_policy
                .disposition(relative_to(&library.root_path, &subtitle))
                == PathDisposition::Sidecar
            {
                self.reconcile_sidecar_event(library, &subtitle, true)
                    .await?;
            }
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
    /// library -- or forget its record when it is gone.
    pub(super) async fn reconcile_nfo_event(
        &self,
        library: &Library,
        path: &Path,
        is_file: bool,
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
        let under = self.file_repo.find_all_under(library.id, dir).await?;
        let candidates: Vec<&MediaFile> = under
            .iter()
            .filter(|file| hints::may_describe(path, &file.path))
            .collect();
        self.reapply_nfo(library, path, stored.as_ref(), &candidates)
            .await
    }
}
