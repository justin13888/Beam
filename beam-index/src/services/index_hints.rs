//! What a library says about its media beside the path (issue #184): the
//! Kodi `.nfo` files next to a video, and the provider id they pin a title to.
//!
//! Beam never writes into a library root. Every NFO is read with a read-only
//! open, at most [`MAX_NFO_BYTES`] of it, and only when it is a regular file:
//! a symbolic link -- the NFO itself or a folder above it beneath the library
//! root -- is not part of the library (issues #186 and #189).
//!
//! An NFO is applied when classification first reads it, and re-applied --
//! by a scan or a watcher event -- exactly when what it holds differs from
//! what was last applied (FR-219), recorded per NFO in `applied_nfos`.

use std::collections::HashSet;
use std::io::Read;

use beam_domain::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::models::movie::Movie;
use beam_domain::models::show::Show;
use beam_domain::models::{PinSource, ProviderPin};
use beam_domain::utils::nfo::{MAX_NFO_BYTES, Nfo, NfoKind, parse_nfo};

use crate::library_file::{FileMeta, open_regular_file, relative_to, stat_regular_file};

use super::*;

/// The NFO a Kodi-style library keeps in a series folder.
pub(super) const TVSHOW_NFO: &str = "tvshow.nfo";
/// The NFO a Kodi-style library keeps in a movie's folder.
pub(super) const MOVIE_NFO: &str = "movie.nfo";

/// An NFO the walk found beside the media, with its stat stamp.
#[derive(Debug, Clone)]
pub(super) struct WalkedNfo {
    pub(super) path: PathBuf,
    pub(super) stamp: Option<String>,
}

/// Whether `path` names an NFO file.
pub(super) fn is_nfo(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("nfo"))
}

/// How long after its last write an NFO's stat stamp is trusted. A write
/// within the same tick of a coarse timestamp as the read that recorded the
/// stamp would leave the stamp unchanged, so a stamp is only recorded once
/// the NFO has not been written for this long; until then the NFO is read
/// again every time (as Git treats a "racily clean" index entry).
const STAMP_SETTLE: chrono::Duration = chrono::Duration::seconds(2);

/// What a stat says about a file, cheaply: its size and its modification and
/// change times. The change time cannot be set by `touch` or `cp -p`, and
/// moves on every write, so an unchanged stamp means an unwritten file.
/// `None` where the platform has no change time: the file is then read every
/// time.
pub(super) fn change_stamp(meta: &FileMeta) -> Option<String> {
    #[cfg(unix)]
    {
        let (mtime, mtime_nsec) = meta.mtime();
        let (ctime, ctime_nsec) = meta.ctime();
        Some(format!(
            "{}:{}.{:09}:{}.{:09}",
            meta.size(),
            mtime,
            mtime_nsec,
            ctime,
            ctime_nsec
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// When a file was last written, by its modification or change time,
/// whichever is later.
fn last_write(meta: &FileMeta) -> Option<DateTime<Utc>> {
    let modified: Option<DateTime<Utc>> = meta.modified().map(Into::into);
    #[cfg(unix)]
    let changed = {
        let (secs, nanos) = meta.ctime();
        DateTime::from_timestamp(secs, nanos)
    };
    #[cfg(not(unix))]
    let changed: Option<DateTime<Utc>> = None;
    modified.max(changed)
}

/// What an NFO held when it was read: enough to tell, next time, whether it
/// changed (FR-219).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NfoContent {
    pub(super) size_bytes: u64,
    /// XXH3-128 of the bytes, as 32 hex digits. Not a security boundary: it
    /// only tells one version of a file from the next.
    pub(super) content_hash: String,
    change_stamp: Option<String>,
    last_write: Option<DateTime<Utc>>,
}

impl NfoContent {
    /// The record of this NFO at `path` in `library_id`, as read at `now`.
    /// The stat stamp is kept only once the NFO has settled
    /// ([`STAMP_SETTLE`]).
    pub(super) fn record(
        &self,
        library_id: Uuid,
        path: &Path,
        now: DateTime<Utc>,
    ) -> RecordAppliedNfo {
        let settled = self
            .last_write
            .is_some_and(|written| written + STAMP_SETTLE <= now);
        RecordAppliedNfo {
            library_id,
            path: path.to_path_buf(),
            size_bytes: self.size_bytes,
            content_hash: self.content_hash.clone(),
            change_stamp: self.change_stamp.clone().filter(|_| settled),
        }
    }

    /// Whether `stored` recorded this same content.
    pub(super) fn same_as(&self, stored: &AppliedNfo) -> bool {
        stored.size_bytes == self.size_bytes && stored.content_hash == self.content_hash
    }
}

/// What reading an NFO found: what it held, and what it says when Beam
/// trusts it (`None` for one that is not UTF-8, declares a document type, or
/// does not parse).
#[derive(Debug)]
pub(super) struct NfoRead {
    pub(super) content: NfoContent,
    pub(super) nfo: Option<Nfo>,
}

#[cfg(test)]
thread_local! {
    /// How many NFOs this thread has read the bytes of: what a test counts to
    /// tell an NFO read from one skipped by its stat stamp.
    pub(super) static NFO_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read the NFO at `path` in the library rooted at `root`: `None` when there
/// is no regular file there, or it is larger than [`MAX_NFO_BYTES`], or
/// cannot be read. Stat'ed and opened read-only never through a link, the
/// NFO's own or a folder's above it beneath the root ([`stat_regular_file`],
/// [`open_regular_file`]); a failure is logged, never raised -- a broken NFO
/// leaves the file classified by its path.
pub(super) fn read_nfo_file(root: &Path, path: &Path) -> Option<NfoRead> {
    let meta = stat_regular_file(root, path).ok()?;
    if meta.size() > MAX_NFO_BYTES {
        warn!(path = %path.display(), bytes = meta.size(), "NFO is larger than Beam reads; ignored");
        return None;
    }
    // The open file is statted before it is read: a write after this stat
    // moves the stamp, so the next read sees it, whereas a stamp taken after
    // the read could vouch for content the read never saw.
    let (file, meta) =
        match relative_to(root, path).and_then(|relative| open_regular_file(root, relative)) {
            Ok(opened) => opened,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "could not open an NFO; ignored");
                return None;
            }
        };
    #[cfg(test)]
    NFO_READS.with(|reads| reads.set(reads.get() + 1));
    let mut bytes = Vec::new();
    if let Err(err) = file.take(MAX_NFO_BYTES + 1).read_to_end(&mut bytes) {
        warn!(path = %path.display(), error = %err, "could not read an NFO; ignored");
        return None;
    }
    if bytes.len() as u64 > MAX_NFO_BYTES {
        warn!(path = %path.display(), "NFO grew past the size Beam reads; ignored");
        return None;
    }
    let meta = FileMeta::from(&meta);
    let content = NfoContent {
        size_bytes: bytes.len() as u64,
        content_hash: format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes)),
        change_stamp: change_stamp(&meta),
        last_write: last_write(&meta),
    };
    let nfo = match parse_nfo(&bytes) {
        Ok(nfo) => Some(nfo),
        Err(err) => {
            warn!(path = %path.display(), error = %err, "NFO is not readable; ignored");
            None
        }
    };
    Some(NfoRead { content, nfo })
}

/// An NFO found describing a video: where it is, what it says, and what it
/// held.
#[derive(Debug)]
pub(super) struct LocatedNfo {
    pub(super) path: PathBuf,
    pub(super) nfo: Nfo,
    pub(super) content: NfoContent,
}

/// Read the NFO at `path`, in the library rooted at `root`, as a located
/// one: `None` unless Beam trusts it.
fn located(root: &Path, path: PathBuf) -> Option<LocatedNfo> {
    let NfoRead { content, nfo } = read_nfo_file(root, &path)?;
    Some(LocatedNfo {
        path,
        nfo: nfo?,
        content,
    })
}

/// The NFOs describing the video at `path` in a library rooted at `root`.
#[derive(Debug, Default)]
pub(super) struct NfoFiles {
    /// `<stem>.nfo` beside the video, else `movie.nfo` in its folder.
    pub(super) file: Option<LocatedNfo>,
    /// `tvshow.nfo` in its folder or, for a file in a season folder, in the
    /// series folder above.
    pub(super) show: Option<LocatedNfo>,
}

/// Whether `folder` may hold an NFO describing media: beneath `root`, never
/// the root itself -- a `movie.nfo` or `tvshow.nfo` there would describe
/// every file in the library.
fn below_root(root: &Path, folder: &Path) -> bool {
    folder != root && folder.starts_with(root)
}

/// Where an NFO describing the video at `path` itself may be, in the order
/// classification reads them: `<stem>.nfo` beside it, then `movie.nfo` in
/// its folder.
fn file_nfo_paths(root: &Path, path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    path.file_stem()
        .map(|stem| dir.join(format!("{}.nfo", stem.to_string_lossy())))
        .into_iter()
        .chain(below_root(root, dir).then(|| dir.join(MOVIE_NFO)))
        .collect()
}

/// The NFO describing the video at `path` itself: the first of
/// [`file_nfo_paths`] Beam can read.
pub(super) fn locate_file_nfo(root: &Path, path: &Path) -> Option<LocatedNfo> {
    file_nfo_paths(root, path)
        .into_iter()
        .find_map(|nfo| located(root, nfo))
}

/// Whether the folder at `dir` is a season folder (`Season 01`, `Specials`),
/// whose show's `tvshow.nfo` lives in the series folder above it.
fn is_season_folder(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .and_then(season_folder_number)
        .is_some()
}

/// Where the `tvshow.nfo` describing the episodes in `dir` may be, in the
/// order classification reads them: in `dir`, then -- only when `dir` is a
/// season folder -- in the series folder above. The folder above a flat
/// show's folder is a category folder (`TV/`), whose NFO would otherwise
/// describe every show beneath it.
fn show_nfo_paths(root: &Path, dir: &Path) -> Vec<PathBuf> {
    [Some(dir), dir.parent().filter(|_| is_season_folder(dir))]
        .into_iter()
        .flatten()
        .filter(|folder| below_root(root, folder))
        .map(|folder| folder.join(TVSHOW_NFO))
        .collect()
}

/// The `tvshow.nfo` describing the episodes in `dir`: the first of
/// [`show_nfo_paths`] Beam can read.
pub(super) fn locate_show_nfo(root: &Path, dir: &Path) -> Option<LocatedNfo> {
    show_nfo_paths(root, dir)
        .into_iter()
        .find_map(|nfo| located(root, nfo))
}

/// Find and read the NFOs describing the video at `path`.
pub(super) fn locate_nfos(root: &Path, path: &Path) -> NfoFiles {
    NfoFiles {
        file: locate_file_nfo(root, path),
        show: path.parent().and_then(|dir| locate_show_nfo(root, dir)),
    }
}

/// Whether the NFO at `nfo_path` could describe the video at `video`, by
/// where the two are: a `tvshow.nfo` the videos in its folder and in the
/// season folders directly below it, any other NFO the videos in its own
/// folder. Whether it *does* is [`locate_nfos`]'s to say.
pub(super) fn may_describe(nfo_path: &Path, video: &Path) -> bool {
    let (Some(dir), Some(video_dir)) = (nfo_path.parent(), video.parent()) else {
        return false;
    };
    video_dir == dir
        || (is_tvshow_nfo(nfo_path)
            && video_dir.parent() == Some(dir)
            && is_season_folder(video_dir))
}

/// Whether `path` names a `tvshow.nfo`, in any case.
pub(super) fn is_tvshow_nfo(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(TVSHOW_NFO))
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
    /// A new file's NFO -- or one a relinked video found beside its new path
    /// that its title never had applied -- disagrees with the title's pin:
    /// keep the pin and tell the administrator (two NFOs name different
    /// titles for one key).
    Keep,
    /// The NFO the pin came from was edited: its new id replaces the old.
    Replace,
}

/// What became of an NFO's pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(super) enum PinOutcome {
    /// Settled: set, already in place, or decided against -- an
    /// administrator's pin, or a conflict the administrator was told of.
    Settled,
    /// Refused by the unique pin: another title holds that id. The NFO is
    /// not recorded as applied, so a later scan tries it again.
    Refused,
}

impl LocalIndexService {
    /// Pin the title `target` -- currently pinned to `current` by
    /// `current_source`, matched to the ids `carried` -- to the NFO's `pin`.
    /// A title newly pinned, or re-pinned, is queued for re-enrichment with
    /// its old match cleared, so the next pass fetches it by the pin. An
    /// administrator's pin is never replaced (FR-312). `source` is the NFO or
    /// video the pin came from, for the administrator.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn apply_pin(
        &self,
        target: EnrichmentTargetId,
        current: Option<&str>,
        current_source: Option<PinSource>,
        carried: bool,
        pin: &ProviderPin,
        conflict: PinConflict,
        source: &Path,
    ) -> Result<PinOutcome, IndexError> {
        let stored = pin.to_ref_string();
        match current {
            Some(current) if current == stored => return Ok(PinOutcome::Settled),
            Some(current) if current_source == Some(PinSource::Admin) => {
                info!(
                    path = %source.display(),
                    pinned = current,
                    nfo = %stored,
                    "an administrator pinned this title; the NFO's pin is not applied"
                );
                return Ok(PinOutcome::Settled);
            }
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
                return Ok(PinOutcome::Settled);
            }
            _ => {}
        }
        let (id, pinned) = match target {
            EnrichmentTargetId::Movie(id) => (
                id,
                self.movie_repo
                    .set_pinned_ref(id, pin, PinSource::Nfo)
                    .await?,
            ),
            EnrichmentTargetId::Show(id) => (
                id,
                self.show_repo
                    .set_pinned_ref(id, pin, PinSource::Nfo)
                    .await?,
            ),
        };
        if !pinned {
            // An administrator's pin was answered above, so this is the
            // unique pin: another title holds the id. (Should an administrator
            // have pinned this title meanwhile, the retry finds that above.)
            warn!(
                path = %source.display(),
                title = %id,
                pin = %stored,
                "another title is already pinned to this id; not pinned, tried again later"
            );
            return Ok(PinOutcome::Refused);
        }
        info!(path = %source.display(), title = %id, pin = %stored, "title pinned by its NFO");
        if !carried && let Some(enrichment_repo) = &self.enrichment_repo {
            enrichment_repo.request_refresh(target, true).await?;
        }
        Ok(PinOutcome::Settled)
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
            // `find_by_pin` just found no other title holding the pin, so only
            // a concurrent writer could have it refused here.
            let _ = self
                .apply_pin(
                    EnrichmentTargetId::Movie(movie.id),
                    movie.pinned_ref.as_deref(),
                    movie.pin_source,
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
            // As in `movie_for`: only a concurrent writer could refuse it.
            let _ = self
                .apply_pin(
                    EnrichmentTargetId::Show(show.id),
                    show.pinned_ref.as_deref(),
                    show.pin_source,
                    carried,
                    pin,
                    PinConflict::Keep,
                    source,
                )
                .await?;
        }
        Ok(show)
    }

    /// Re-apply the pin of the NFO at `nfo_path`, which says `nfo` and changed
    /// since it was last applied, to the titles of the indexed `files` it is
    /// *the* NFO of: those whose own NFO, located exactly as classification
    /// locates it ([`locate_nfos`]), is this one. So a `tvshow.nfo` re-pins
    /// the shows of the episodes whose `tvshow.nfo` it is, a `movie.nfo` the
    /// movies in its folder with no `<stem>.nfo` of their own, and a
    /// `<stem>.nfo` the movie of its video; an NFO at the library root, or
    /// further above a video than classification looks, re-pins nothing.
    /// With [`PinConflict::Replace`] the NFO's pin replaces the one it set
    /// before; with [`PinConflict::Keep`] -- an NFO a relinked video found
    /// beside its new path, never applied to its title -- a title's other pin
    /// is kept, as classification keeps it. Never an administrator's
    /// (FR-312). An NFO that pins nothing leaves the pin as it is: the title
    /// stays fetched by the id it was last given.
    /// [`PinOutcome::Refused`] when the unique pin refused any of the titles
    /// because a title this NFO does not describe holds the id. When the
    /// holder is another of its own titles -- a `movie.nfo` beside two
    /// movies, a `tvshow.nfo` over episodes of two shows -- no retry could
    /// ever succeed, so the NFO is settled and the administrator told once.
    pub(super) async fn repin_from_nfo(
        &self,
        library: &Library,
        nfo_path: &Path,
        nfo: &Nfo,
        files: &[&MediaFile],
        conflict: PinConflict,
    ) -> Result<PinOutcome, IndexError> {
        let mut outcome = PinOutcome::Settled;
        let Some(pin) = nfo.ids.pin() else {
            return Ok(outcome);
        };
        let describes = |wanted: NfoKind| nfo.kind.is_none_or(|kind| kind == wanted);
        let root = library.root_path.as_path();
        let is_this = |located: Option<LocatedNfo>| located.is_some_and(|l| l.path == nfo_path);

        // Ordered by id: when the NFO describes several titles, the one with
        // the lowest id takes its pin, the same one at every scan.
        let mut targets: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
        if is_tvshow_nfo(nfo_path) {
            if !describes(NfoKind::TvShow) {
                return Ok(outcome);
            }
            // Every episode in one folder locates the same `tvshow.nfo`.
            let mut located_here: HashMap<&Path, bool> = HashMap::new();
            for file in files {
                let Some(MediaFileContent::Episode { episode_id, .. }) = file.content else {
                    continue;
                };
                let Some(dir) = file.path.parent() else {
                    continue;
                };
                if !*located_here
                    .entry(dir)
                    .or_insert_with(|| is_this(locate_show_nfo(root, dir)))
                {
                    continue;
                }
                let Some(episode) = self.show_repo.find_episode_by_id(episode_id).await? else {
                    continue;
                };
                if let Some(season) = self.show_repo.find_season_by_id(episode.season_id).await? {
                    targets.insert(season.show_id);
                }
            }
            let mut shares_pin = false;
            for &show_id in &targets {
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
                if self
                    .apply_pin(
                        EnrichmentTargetId::Show(show.id),
                        show.pinned_ref.as_deref(),
                        show.pin_source,
                        carried,
                        &pin,
                        conflict,
                        nfo_path,
                    )
                    .await?
                    == PinOutcome::Refused
                {
                    // Held by another of this NFO's own titles: no retry can
                    // free it, so the NFO is settled and the administrator
                    // told. Held by a title outside it: tried again later.
                    let holder = self.show_repo.find_by_pin(&pin).await?;
                    if holder.is_some_and(|holder| targets.contains(&holder.id)) {
                        shares_pin = true;
                    } else {
                        outcome = PinOutcome::Refused;
                    }
                }
            }
            if shares_pin {
                self.log_nfo_describes_several_titles(nfo_path, &pin, "shows")
                    .await;
            }
            return Ok(outcome);
        }

        if !describes(NfoKind::Movie) {
            return Ok(outcome);
        }
        for file in files {
            let Some(MediaFileContent::Movie { movie_entry_id }) = file.content else {
                continue;
            };
            if !is_this(locate_file_nfo(root, &file.path)) {
                continue;
            }
            if let Some(entry) = self.movie_repo.find_entry_by_id(movie_entry_id).await? {
                targets.insert(entry.movie_id);
            }
        }
        let mut shares_pin = false;
        for &movie_id in &targets {
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
            if self
                .apply_pin(
                    EnrichmentTargetId::Movie(movie.id),
                    movie.pinned_ref.as_deref(),
                    movie.pin_source,
                    carried,
                    &pin,
                    conflict,
                    nfo_path,
                )
                .await?
                == PinOutcome::Refused
            {
                // As for shows above.
                let holder = self.movie_repo.find_by_pin(&pin).await?;
                if holder.is_some_and(|holder| targets.contains(&holder.id)) {
                    shares_pin = true;
                } else {
                    outcome = PinOutcome::Refused;
                }
            }
        }
        if shares_pin {
            self.log_nfo_describes_several_titles(nfo_path, &pin, "movies")
                .await;
        }
        Ok(outcome)
    }

    /// Tell the administrator, once per application, that the NFO at
    /// `nfo_path` describes several `titles` (`"movies"` or `"shows"`) and
    /// only one of them could take its `pin`: an id pins one title, so the
    /// others keep the pin they had.
    async fn log_nfo_describes_several_titles(
        &self,
        nfo_path: &Path,
        pin: &ProviderPin,
        titles: &str,
    ) {
        let stored = pin.to_ref_string();
        warn!(
            path = %nfo_path.display(),
            pin = %stored,
            "an NFO describes several titles; only one of them is pinned to its id"
        );
        let _ = self
            .admin_log
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::LibraryScan,
                format!(
                    "An NFO pins several {titles} to {stored}; an id pins one title, so only one \
                     of them is pinned: {}",
                    nfo_path.display()
                ),
                Some(serde_json::json!({
                    "path": nfo_path.display().to_string(),
                    "nfo": stored,
                })),
            )
            .await;
    }

    /// Record, as applied, the NFOs classification just read for a file
    /// (FR-219). An NFO seen for the first time is recorded as it is: what
    /// classification did with it -- pinning a new title, or keeping a
    /// title's pin against it and telling the administrator -- is its
    /// application, and a later scan must not apply it again with
    /// [`PinConflict::Replace`]. An NFO already recorded with other content
    /// was edited since it was applied, and is left for the scan's or the
    /// watcher's re-apply to replace the pin with.
    pub(super) async fn record_consumed_nfos(
        &self,
        library: &Library,
        consumed: &[&LocatedNfo],
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.applied_nfo_repo else {
            return Ok(());
        };
        for located in consumed {
            let LocatedNfo {
                path,
                nfo: _,
                content,
            } = located;
            let stored = repo.find_by_path(path).await?;
            if stored
                .as_ref()
                .is_some_and(|stored| !content.same_as(stored))
            {
                continue;
            }
            let record = content.record(library.id, path, self.clock.now());
            if !stored.as_ref().is_some_and(|stored| record.matches(stored)) {
                repo.record_by_path(record).await?;
            }
        }
        Ok(())
    }

    /// Re-read the NFO at `path` -- whose record is `stored` -- and, when its
    /// content differs from what was last applied, re-apply it to those of
    /// `files` it describes; then record what it holds. An NFO that cannot be
    /// read is neither applied nor recorded, so it is tried again; so is one
    /// whose pin another title holds ([`PinOutcome::Refused`]).
    pub(super) async fn reapply_nfo(
        &self,
        library: &Library,
        path: &Path,
        stored: Option<&AppliedNfo>,
        files: &[&MediaFile],
    ) -> Result<(), IndexError> {
        let Some(read) = read_nfo_file(&library.root_path, path) else {
            return Ok(());
        };
        self.reapply_read_nfo(library, path, stored, files, read)
            .await
    }

    /// [`Self::reapply_nfo`], for an NFO the caller has already read.
    pub(super) async fn reapply_read_nfo(
        &self,
        library: &Library,
        path: &Path,
        stored: Option<&AppliedNfo>,
        files: &[&MediaFile],
        read: NfoRead,
    ) -> Result<(), IndexError> {
        let NfoRead { content, nfo } = read;
        if !stored.is_some_and(|stored| content.same_as(stored))
            && let Some(nfo) = &nfo
            && self
                .repin_from_nfo(library, path, nfo, files, PinConflict::Replace)
                .await?
                == PinOutcome::Refused
        {
            return Ok(());
        }
        if let Some(repo) = &self.applied_nfo_repo {
            let record = content.record(library.id, path, self.clock.now());
            if !stored.is_some_and(|stored| record.matches(stored)) {
                repo.record_by_path(record).await?;
            }
        }
        Ok(())
    }

    /// Carry the applied state of the NFOs of the videos one scan or one
    /// watcher event just relinked (issue #180) -- each `(from, moved)` a
    /// video relinked from `from` to `moved`'s path, which is never
    /// classified again -- to the NFOs classification would read for each of
    /// them now: a movie's own, an episode's show's and its own (FR-219). A
    /// later scan or watcher event then does not take a moved NFO for a new
    /// or an edited one and replace the title's pin with it.
    ///
    /// The batch is judged as a whole, against the records as they stood
    /// before any is written, so the outcome never turns on the order the
    /// videos come in -- nor on how the folders of a swap or a rotation
    /// sort. One rule covers a move, a swap and a rotation: an NFO whose
    /// content its path's record does not hold (or that has no record) has
    /// *moved* when that content is what the record of another NFO path the
    /// relinked videos had holds, and the file there no longer holds it --
    /// gone, or holding other content. A moved NFO takes that record's
    /// applied state: it is recorded as it is, and changes no pin. An NFO
    /// several of the videos locate -- a folder's `movie.nfo`, a show's
    /// `tvshow.nfo` -- is judged once, over all of them. An NFO whose
    /// content no such record holds is judged by itself:
    ///
    /// * one its path's record already holds is settled and left alone -- a
    ///   video moved beside it does not take it;
    /// * one whose path has a record of other content was edited, and is
    ///   left for the re-apply to replace the pin with;
    /// * one with no record, where an NFO any video locating it had at its
    ///   `from` is gone from disk and recorded, was edited during the move:
    ///   left unrecorded, so the re-apply replaces the pin with it;
    /// * any other with no record -- an NFO the title never had, or one
    ///   whose record a removal forgot first -- is applied to every video
    ///   locating it as classification applies a new file's, keeping a
    ///   title's other pin, then recorded.
    pub(super) async fn carry_nfos_on_relink(
        &self,
        library: &Library,
        relinked: &[(PathBuf, MediaFile)],
    ) -> Result<(), IndexError> {
        let Some(repo) = &self.applied_nfo_repo else {
            return Ok(());
        };
        let root = library.root_path.as_path();
        // Each relinked video's NFOs, each with the paths its counterpart at
        // `from` could have had, in the order classification would have
        // located it there.
        let mut located: Vec<(&MediaFile, LocatedNfo, Vec<PathBuf>)> = Vec::new();
        for (from, moved) in relinked {
            let (Some(old_dir), Some(new_dir)) = (from.parent(), moved.path.parent()) else {
                continue;
            };
            let old_file_nfos = || {
                let mut paths: Vec<PathBuf> = from
                    .file_stem()
                    .map(|stem| old_dir.join(format!("{}.nfo", stem.to_string_lossy())))
                    .into_iter()
                    .collect();
                paths.push(old_dir.join(MOVIE_NFO));
                paths
            };
            let old_show_nfos = || {
                let mut paths = vec![old_dir.join(TVSHOW_NFO)];
                if is_season_folder(old_dir)
                    && let Some(above) = old_dir.parent()
                {
                    paths.push(above.join(TVSHOW_NFO));
                }
                paths
            };
            match moved.content {
                Some(MediaFileContent::Movie { .. }) => {
                    if let Some(nfo) = locate_file_nfo(root, &moved.path) {
                        located.push((moved, nfo, old_file_nfos()));
                    }
                }
                Some(MediaFileContent::Episode { .. }) => {
                    if let Some(nfo) = locate_show_nfo(root, new_dir) {
                        located.push((moved, nfo, old_show_nfos()));
                    }
                    if let Some(nfo) = locate_file_nfo(root, &moved.path) {
                        located.push((moved, nfo, old_file_nfos()));
                    }
                }
                None => {}
            }
        }

        // The record of every path involved, read before any is written.
        let mut records: HashMap<PathBuf, Option<AppliedNfo>> = HashMap::new();
        for (_, located, before) in &located {
            for path in std::iter::once(&located.path).chain(before) {
                if !records.contains_key(path) {
                    records.insert(path.clone(), repo.find_by_path(path).await?);
                }
            }
        }
        let record_of = |path: &Path| records.get(path).and_then(Option::as_ref);
        // The records of the NFOs the relinked videos had whose files no
        // longer hold what they record -- gone, or holding other content:
        // the content they record may have moved.
        let mut vacated: Vec<&AppliedNfo> = Vec::new();
        let mut looked_at: HashSet<&Path> = HashSet::new();
        for (_, _, before) in &located {
            for old in before {
                if !looked_at.insert(old.as_path()) {
                    continue;
                }
                let Some(record) = record_of(old) else {
                    continue;
                };
                if path_is_absent(old)
                    || read_nfo_file(root, old).is_some_and(|read| !read.content.same_as(record))
                {
                    vacated.push(record);
                }
            }
        }

        // An NFO several relinked videos share -- a folder's `movie.nfo`, a
        // show's `tvshow.nfo` -- is judged once, over all of them: every
        // video that locates it, and every path their counterparts at
        // `from` could have had. Ordered by path, so the NFOs of one batch
        // are judged in the same order whatever order the videos came in.
        let mut shared: std::collections::BTreeMap<
            &Path,
            (&LocatedNfo, Vec<&MediaFile>, Vec<&Path>),
        > = std::collections::BTreeMap::new();
        for (moved, located, before) in &located {
            let (_, videos, olds) = shared
                .entry(located.path.as_path())
                .or_insert_with(|| (located, Vec::new(), Vec::new()));
            videos.push(*moved);
            olds.extend(before.iter().map(PathBuf::as_path));
        }
        for (located, videos, before) in shared.into_values() {
            let LocatedNfo { path, nfo, content } = located;
            let stored = record_of(path);
            if stored.is_some_and(|stored| content.same_as(stored)) {
                continue;
            }
            let moved_here = vacated
                .iter()
                .any(|old| old.path != *path && content.same_as(old));
            if !moved_here {
                if stored.is_some() {
                    continue;
                }
                let edited_in_the_move = before
                    .iter()
                    .any(|old| record_of(old).is_some() && path_is_absent(old));
                if edited_in_the_move {
                    continue;
                }
                if self
                    .repin_from_nfo(library, path, nfo, &videos, PinConflict::Keep)
                    .await?
                    == PinOutcome::Refused
                {
                    continue;
                }
            }
            repo.record_by_path(content.record(library.id, path, self.clock.now()))
                .await?;
        }
        Ok(())
    }

    /// Clear the administrator's pin on the title `target` (issue #185) and
    /// pin it by its NFO again, as classification would pin it now -- or,
    /// with none that pins it, leave it to be found by its path. Returns
    /// whether there was an administrator's pin to clear.
    ///
    /// The NFOs are those classification locates for the title's files (a
    /// movie's own, an episode's show's), read afresh, in path order; the
    /// first that pins the title settles it. The steps are ordered so that no
    /// failure strands the title: every NFO the title's files could have is
    /// first forgotten as applied, then the pin is cleared, then the NFOs
    /// read are applied and recorded again. So an NFO that cannot be read
    /// now -- a share offline -- or whose id another title holds stays
    /// forgotten, and the next scan or watcher event applies it afresh; and
    /// should a step fail, the administrator's pin is still in place (which
    /// no NFO replaces) or the NFOs are left for that scan to apply.
    pub async fn release_admin_pin(&self, target: EnrichmentTargetId) -> Result<bool, IndexError> {
        let mut files: Vec<MediaFile> = Vec::new();
        match target {
            EnrichmentTargetId::Movie(id) => {
                for entry in self.movie_repo.find_entries_by_movie_id(id).await? {
                    files.extend(self.file_repo.find_by_movie_entry_id(entry.id).await?);
                }
            }
            EnrichmentTargetId::Show(id) => {
                for season in self.show_repo.find_seasons_by_show_id(id).await? {
                    for episode in self.show_repo.find_episodes_by_season_id(season.id).await? {
                        files.extend(self.file_repo.find_by_episode_id(episode.id).await?);
                    }
                }
            }
        }

        // Every path an NFO describing the title's files may be at, and the
        // NFOs Beam reads there now, by path, with the library and the files
        // locating each.
        let mut libraries: HashMap<Uuid, Option<Library>> = HashMap::new();
        let mut paths: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        let mut nfos: std::collections::BTreeMap<PathBuf, (Library, LocatedNfo, Vec<&MediaFile>)> =
            std::collections::BTreeMap::new();
        for file in &files {
            if let std::collections::hash_map::Entry::Vacant(slot) =
                libraries.entry(file.library_id)
            {
                slot.insert(self.library_repo.find_by_id(file.library_id).await?);
            }
            let Some(Some(library)) = libraries.get(&file.library_id) else {
                continue;
            };
            let root = library.root_path.as_path();
            let candidates = match target {
                EnrichmentTargetId::Movie(_) => file_nfo_paths(root, &file.path),
                EnrichmentTargetId::Show(_) => file
                    .path
                    .parent()
                    .map(|dir| show_nfo_paths(root, dir))
                    .unwrap_or_default(),
            };
            paths.extend(candidates.iter().cloned());
            let Some(read) = candidates.into_iter().find_map(|nfo| located(root, nfo)) else {
                continue;
            };
            nfos.entry(read.path.clone())
                .or_insert_with(|| (library.clone(), read, Vec::new()))
                .2
                .push(file);
        }

        // Forgotten first: whatever happens next, the next scan or watcher
        // event applies each afresh.
        if let Some(repo) = &self.applied_nfo_repo {
            let mut forgotten = Vec::new();
            for path in &paths {
                if let Some(record) = repo.find_by_path(path).await? {
                    forgotten.push(record.id);
                }
            }
            repo.delete_by_ids(forgotten).await?;
        }

        let cleared = match target {
            EnrichmentTargetId::Movie(id) => self.movie_repo.clear_admin_pin(id).await?,
            EnrichmentTargetId::Show(id) => self.show_repo.clear_admin_pin(id).await?,
        };
        if !cleared {
            return Ok(false);
        }

        let mut settled = false;
        for (path, (library, read, located_by)) in &nfos {
            let LocatedNfo {
                path: _,
                nfo,
                content,
            } = read;
            if !settled && nfo.ids.pin().is_some() {
                // `Keep`: the title has no pin now, so the NFO's is set;
                // should a concurrent writer have pinned it meanwhile, that
                // pin stands.
                match self
                    .repin_from_nfo(library, path, nfo, located_by, PinConflict::Keep)
                    .await?
                {
                    PinOutcome::Settled => settled = true,
                    // Another title holds the id: left forgotten, so it is
                    // tried again.
                    PinOutcome::Refused => continue,
                }
            }
            if let Some(repo) = &self.applied_nfo_repo {
                repo.record_by_path(content.record(library.id, path, self.clock.now()))
                    .await?;
            }
        }
        Ok(true)
    }
}
