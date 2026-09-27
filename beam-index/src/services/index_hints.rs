//! What a library says about its media beside the path (issue #184): the
//! Kodi `.nfo` files next to a video, and the provider id they pin a title to.
//!
//! Beam never writes into a library root. Every NFO is read with a read-only
//! open, at most [`MAX_NFO_BYTES`] of it, and only when it is a regular file:
//! a symbolic link is not part of the library (issue #186).

use std::io::Read;

use beam_domain::models::ProviderPin;
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::models::movie::Movie;
use beam_domain::models::show::Show;
use beam_domain::utils::nfo::{MAX_NFO_BYTES, Nfo, NfoKind, parse_nfo};

use super::*;

/// The NFO a Kodi-style library keeps in a series folder.
pub(super) const TVSHOW_NFO: &str = "tvshow.nfo";
/// The NFO a Kodi-style library keeps in a movie's folder.
pub(super) const MOVIE_NFO: &str = "movie.nfo";

/// Whether `path` names an NFO file.
pub(super) fn is_nfo(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("nfo"))
}

/// Read the NFO at `path`: `None` when there is no regular file there, or it
/// is larger than [`MAX_NFO_BYTES`], cannot be read, or is not an NFO Beam
/// trusts. Opened read-only; a failure is logged, never raised -- a broken
/// NFO leaves the file classified by its path.
pub(super) fn read_nfo(path: &Path) -> Option<Nfo> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_NFO_BYTES {
        warn!(path = %path.display(), bytes = meta.len(), "NFO is larger than Beam reads; ignored");
        return None;
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_NFO_BYTES + 1).read_to_end(&mut bytes));
    if let Err(err) = read {
        warn!(path = %path.display(), error = %err, "could not read an NFO; ignored");
        return None;
    }
    if bytes.len() as u64 > MAX_NFO_BYTES {
        warn!(path = %path.display(), "NFO grew past the size Beam reads; ignored");
        return None;
    }
    match parse_nfo(&bytes) {
        Ok(nfo) => Some(nfo),
        Err(err) => {
            warn!(path = %path.display(), error = %err, "NFO is not readable; ignored");
            None
        }
    }
}

/// The NFOs describing the video at `path` in a library rooted at `root`.
#[derive(Debug, Default)]
pub(super) struct NfoFiles {
    /// `<stem>.nfo` beside the video, else `movie.nfo` in its folder.
    pub(super) file: Option<Nfo>,
    /// `tvshow.nfo` in its folder or the folder above -- the series folder
    /// of a file in a season folder.
    pub(super) show: Option<Nfo>,
}

/// Find and read the NFOs describing the video at `path`. A folder is never
/// the library root itself: a `movie.nfo` or `tvshow.nfo` at the root would
/// describe every file in the library.
pub(super) fn locate_nfos(root: &Path, path: &Path) -> NfoFiles {
    let Some(dir) = path.parent() else {
        return NfoFiles::default();
    };
    let below_root = |folder: &Path| folder != root && folder.starts_with(root);
    let file = path
        .file_stem()
        .map(|stem| dir.join(format!("{}.nfo", stem.to_string_lossy())))
        .and_then(|own| read_nfo(&own))
        .or_else(|| {
            below_root(dir)
                .then(|| read_nfo(&dir.join(MOVIE_NFO)))
                .flatten()
        });
    let show = [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .filter(|folder| below_root(folder))
        .find_map(|folder| read_nfo(&folder.join(TVSHOW_NFO)));
    NfoFiles { file, show }
}

/// Whether a title enrichment already matched to `pin`'s id needs no refresh
/// to be fetched by it.
fn carries(
    pin: &ProviderPin,
    tmdb_id: Option<u32>,
    imdb_id: Option<&str>,
    tvdb_id: Option<u32>,
    anilist_id: Option<u32>,
) -> bool {
    match pin {
        ProviderPin::Tmdb(id) => tmdb_id == Some(*id),
        ProviderPin::Imdb(id) => imdb_id == Some(id.as_str()),
        ProviderPin::Tvdb(id) => tvdb_id == Some(*id),
        ProviderPin::Anilist(id) => anilist_id == Some(*id),
    }
}

/// What to do when a title already carries another pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PinConflict {
    /// A new file's NFO disagrees with the title's pin: keep the pin and tell
    /// the administrator (two NFOs name different titles for one key).
    Keep,
    /// The NFO the pin came from was edited: its new id replaces the old.
    Replace,
}

impl LocalIndexService {
    /// Pin the title `target` -- currently pinned to `current`, matched to
    /// the ids `carried` -- to `pin`. A title newly pinned, or re-pinned, is
    /// queued for re-enrichment with its old match cleared, so the next pass
    /// fetches it by the pin. `source` is the NFO or video the pin came from,
    /// for the administrator.
    pub(super) async fn apply_pin(
        &self,
        target: EnrichmentTargetId,
        current: Option<&str>,
        carried: bool,
        pin: &ProviderPin,
        conflict: PinConflict,
        source: &Path,
    ) -> Result<(), IndexError> {
        let stored = pin.to_ref_string();
        match current {
            Some(current) if current == stored => return Ok(()),
            Some(current) if conflict == PinConflict::Keep => {
                warn!(
                    path = %source.display(),
                    pinned = current,
                    nfo = %stored,
                    "an NFO pins a title to another id than it is pinned to; the pin is kept"
                );
                let _ = self
                    .admin_log
                    .log(
                        AdminLogLevel::Warning,
                        AdminLogCategory::LibraryScan,
                        format!(
                            "An NFO pins a title to {stored}, but the title is already pinned to \
                             {current}; the existing pin is kept: {}",
                            source.display()
                        ),
                        Some(serde_json::json!({
                            "path": source.display().to_string(),
                            "pinned": current,
                            "nfo": stored,
                        })),
                    )
                    .await;
                return Ok(());
            }
            _ => {}
        }
        let (id, pinned) = match target {
            EnrichmentTargetId::Movie(id) => (id, self.movie_repo.set_pinned_ref(id, pin).await?),
            EnrichmentTargetId::Show(id) => (id, self.show_repo.set_pinned_ref(id, pin).await?),
        };
        if !pinned {
            warn!(
                path = %source.display(),
                title = %id,
                pin = %stored,
                "another title is already pinned to this id; not pinned"
            );
            return Ok(());
        }
        info!(path = %source.display(), title = %id, pin = %stored, "title pinned by its NFO");
        if !carried && let Some(enrichment_repo) = &self.enrichment_repo {
            enrichment_repo.request_refresh(target, true).await?;
        }
        Ok(())
    }

    /// The movie a file is attached to: the one `pin` names, if one does,
    /// else the one `create` keys (created if new), pinned to `pin`.
    pub(super) async fn movie_for(
        &self,
        create: CreateMovie,
        pin: Option<&ProviderPin>,
        source: &Path,
    ) -> Result<Movie, IndexError> {
        let pinned = match pin {
            Some(pin) => self.movie_repo.find_by_pin(pin).await?,
            None => None,
        };
        let movie = match pinned {
            Some(movie) => movie,
            None => self.movie_repo.find_or_create_by_identity(create).await?,
        };
        if let Some(enrichment_repo) = &self.enrichment_repo {
            enrichment_repo
                .ensure_pending(EnrichmentTargetId::Movie(movie.id))
                .await?;
        }
        if let Some(pin) = pin {
            let carried = carries(
                pin,
                movie.tmdb_id,
                movie.imdb_id.as_deref(),
                movie.tvdb_id,
                movie.anilist_id,
            );
            self.apply_pin(
                EnrichmentTargetId::Movie(movie.id),
                movie.pinned_ref.as_deref(),
                carried,
                pin,
                PinConflict::Keep,
                source,
            )
            .await?;
        }
        Ok(movie)
    }

    /// The show an episode file is attached to; see [`Self::movie_for`].
    pub(super) async fn show_for(
        &self,
        create: CreateShow,
        pin: Option<&ProviderPin>,
        source: &Path,
    ) -> Result<Show, IndexError> {
        let pinned = match pin {
            Some(pin) => self.show_repo.find_by_pin(pin).await?,
            None => None,
        };
        let show = match pinned {
            Some(show) => show,
            None => self.show_repo.find_or_create_by_identity(create).await?,
        };
        if let Some(enrichment_repo) = &self.enrichment_repo {
            enrichment_repo
                .ensure_pending(EnrichmentTargetId::Show(show.id))
                .await?;
        }
        if let Some(pin) = pin {
            let carried = carries(
                pin,
                show.tmdb_id,
                show.imdb_id.as_deref(),
                show.tvdb_id,
                show.anilist_id,
            );
            self.apply_pin(
                EnrichmentTargetId::Show(show.id),
                show.pinned_ref.as_deref(),
                carried,
                pin,
                PinConflict::Keep,
                source,
            )
            .await?;
        }
        Ok(show)
    }

    /// Re-apply the pin of the NFO at `nfo_path`, which changed since it was
    /// last read, to the titles of the indexed `files` it describes: a
    /// `tvshow.nfo` the shows of the episodes under its folder, a `movie.nfo`
    /// the movies in its folder, a `<stem>.nfo` the movie of the video of that
    /// stem. The NFO's pin replaces the one it set before. An NFO that no
    /// longer pins anything leaves the pin as it is: the title stays fetched
    /// by the id it was last given.
    pub(super) async fn repin_from_nfo(
        &self,
        nfo_path: &Path,
        files: &[MediaFile],
    ) -> Result<(), IndexError> {
        let (Some(dir), Some(name)) = (nfo_path.parent(), nfo_path.file_name()) else {
            return Ok(());
        };
        let name = name.to_string_lossy().to_lowercase();
        let Some(nfo) = read_nfo(nfo_path) else {
            return Ok(());
        };
        let Some(pin) = nfo.ids.pin() else {
            return Ok(());
        };
        let describes = |wanted: NfoKind| nfo.kind.is_none_or(|kind| kind == wanted);

        let mut targets: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
        if name == TVSHOW_NFO {
            if !describes(NfoKind::TvShow) {
                return Ok(());
            }
            for file in files.iter().filter(|f| f.path.starts_with(dir)) {
                let Some(MediaFileContent::Episode { episode_id, .. }) = file.content else {
                    continue;
                };
                let Some(episode) = self.show_repo.find_episode_by_id(episode_id).await? else {
                    continue;
                };
                if let Some(season) = self.show_repo.find_season_by_id(episode.season_id).await? {
                    targets.insert(season.show_id);
                }
            }
            for show_id in targets {
                let Some(show) = self.show_repo.find_by_id(show_id).await? else {
                    continue;
                };
                let carried = carries(
                    &pin,
                    show.tmdb_id,
                    show.imdb_id.as_deref(),
                    show.tvdb_id,
                    show.anilist_id,
                );
                self.apply_pin(
                    EnrichmentTargetId::Show(show.id),
                    show.pinned_ref.as_deref(),
                    carried,
                    &pin,
                    PinConflict::Replace,
                    nfo_path,
                )
                .await?;
            }
            return Ok(());
        }

        if !describes(NfoKind::Movie) {
            return Ok(());
        }
        let own_stem = (name != MOVIE_NFO).then(|| nfo_path.file_stem()).flatten();
        for file in files.iter().filter(|f| f.path.parent() == Some(dir)) {
            if own_stem.is_some_and(|stem| file.path.file_stem() != Some(stem)) {
                continue;
            }
            let Some(MediaFileContent::Movie { movie_entry_id }) = file.content else {
                continue;
            };
            if let Some(entry) = self.movie_repo.find_entry_by_id(movie_entry_id).await? {
                targets.insert(entry.movie_id);
            }
        }
        for movie_id in targets {
            let Some(movie) = self.movie_repo.find_by_id(movie_id).await? else {
                continue;
            };
            let carried = carries(
                &pin,
                movie.tmdb_id,
                movie.imdb_id.as_deref(),
                movie.tvdb_id,
                movie.anilist_id,
            );
            self.apply_pin(
                EnrichmentTargetId::Movie(movie.id),
                movie.pinned_ref.as_deref(),
                carried,
                &pin,
                PinConflict::Replace,
                nfo_path,
            )
            .await?;
        }
        Ok(())
    }
}
