//! What a library says about its media beside the path (issue #184): the
//! Kodi `.nfo` files next to a video, and the provider id they pin a title to.
//!
//! Beam never writes into a library root. Every NFO is read with a read-only
//! open, at most [`MAX_NFO_BYTES`] of it, and only when it is a regular file:
//! a symbolic link is not part of the library (issue #186).
//!
//! An NFO is applied when classification first reads it, and re-applied --
//! by a scan or a watcher event -- exactly when what it holds differs from
//! what was last applied (FR-219), recorded per NFO in `applied_nfos`.

use std::io::Read;

use beam_domain::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::models::movie::Movie;
use beam_domain::models::show::Show;
use beam_domain::models::{PinSource, ProviderPin};
use beam_domain::utils::nfo::{MAX_NFO_BYTES, Nfo, NfoKind, parse_nfo};

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
pub(super) fn change_stamp(meta: &std::fs::Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "{}:{}.{:09}:{}.{:09}",
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
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
fn last_write(meta: &std::fs::Metadata) -> Option<DateTime<Utc>> {
    let modified: Option<DateTime<Utc>> = meta.modified().ok().map(Into::into);
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        DateTime::from_timestamp(meta.ctime(), meta.ctime_nsec().clamp(0, 999_999_999) as u32)
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

/// Open `path` for reading, never through a symbolic link: on Unix with
/// `O_NOFOLLOW`, so a link swapped in after the caller's `lstat` fails to open
/// rather than being followed out of the library (issue #186).
pub(super) fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    options.open(path)
}

/// Read the NFO at `path`: `None` when there is no regular file there, or it
/// is larger than [`MAX_NFO_BYTES`], or cannot be read. Opened read-only and
/// never through a link; a failure is logged, never raised -- a broken NFO
/// leaves the file classified by its path.
pub(super) fn read_nfo_file(path: &Path) -> Option<NfoRead> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_NFO_BYTES {
        warn!(path = %path.display(), bytes = meta.len(), "NFO is larger than Beam reads; ignored");
        return None;
    }
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "could not open an NFO; ignored");
            return None;
        }
    };
    // Stat the open file before reading it: a write after this stat moves the
    // stamp, so the next read sees it, whereas a stamp taken after the read
    // could vouch for content the read never saw.
    let meta = match file.metadata() {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return None,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "could not stat an NFO; ignored");
            return None;
        }
    };
    let mut bytes = Vec::new();
    if let Err(err) = file.take(MAX_NFO_BYTES + 1).read_to_end(&mut bytes) {
        warn!(path = %path.display(), error = %err, "could not read an NFO; ignored");
        return None;
    }
    if bytes.len() as u64 > MAX_NFO_BYTES {
        warn!(path = %path.display(), "NFO grew past the size Beam reads; ignored");
        return None;
    }
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

/// Read the NFO at `path` as a located one: `None` unless Beam trusts it.
fn located(path: PathBuf) -> Option<LocatedNfo> {
    let NfoRead { content, nfo } = read_nfo_file(&path)?;
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
    /// `tvshow.nfo` in its folder or the folder above -- the series folder
    /// of a file in a season folder.
    pub(super) show: Option<LocatedNfo>,
}

/// Whether `folder` may hold an NFO describing media: beneath `root`, never
/// the root itself -- a `movie.nfo` or `tvshow.nfo` there would describe
/// every file in the library.
fn below_root(root: &Path, folder: &Path) -> bool {
    folder != root && folder.starts_with(root)
}

/// The NFO describing the video at `path` itself: `<stem>.nfo` beside it,
/// else `movie.nfo` in its folder.
pub(super) fn locate_file_nfo(root: &Path, path: &Path) -> Option<LocatedNfo> {
    let dir = path.parent()?;
    path.file_stem()
        .map(|stem| dir.join(format!("{}.nfo", stem.to_string_lossy())))
        .and_then(located)
        .or_else(|| {
            below_root(root, dir)
                .then(|| located(dir.join(MOVIE_NFO)))
                .flatten()
        })
}

/// The `tvshow.nfo` describing the episodes in `dir`: in `dir`, else in the
/// folder above.
pub(super) fn locate_show_nfo(root: &Path, dir: &Path) -> Option<LocatedNfo> {
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .filter(|folder| below_root(root, folder))
        .find_map(|folder| located(folder.join(TVSHOW_NFO)))
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
/// folders one level below it, any other NFO the videos in its own folder.
/// Whether it *does* is [`locate_nfos`]'s to say.
pub(super) fn may_describe(nfo_path: &Path, video: &Path) -> bool {
    let (Some(dir), Some(video_dir)) = (nfo_path.parent(), video.parent()) else {
        return false;
    };
    video_dir == dir || (is_tvshow_nfo(nfo_path) && video_dir.parent() == Some(dir))
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
    /// A new file's NFO disagrees with the title's pin: keep the pin and tell
    /// the administrator (two NFOs name different titles for one key).
    Keep,
    /// The NFO the pin came from was edited: its new id replaces the old.
    Replace,
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
    ) -> Result<(), IndexError> {
        let stored = pin.to_ref_string();
        match current {
            Some(current) if current == stored => return Ok(()),
            Some(current) if current_source == Some(PinSource::Admin) => {
                info!(
                    path = %source.display(),
                    pinned = current,
                    nfo = %stored,
                    "an administrator pinned this title; the NFO's pin is not applied"
                );
                return Ok(());
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
                return Ok(());
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
            warn!(
                path = %source.display(),
                title = %id,
                pin = %stored,
                "another title is already pinned to this id, or an administrator pinned \
                 this one; not pinned"
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
            self.apply_pin(
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
    /// further above a video than classification looks, re-pins nothing. The
    /// NFO's pin replaces the one it set before, but never an
    /// administrator's (FR-312). An NFO that pins nothing leaves the pin as
    /// it is: the title stays fetched by the id it was last given.
    pub(super) async fn repin_from_nfo(
        &self,
        library: &Library,
        nfo_path: &Path,
        nfo: &Nfo,
        files: &[&MediaFile],
    ) -> Result<(), IndexError> {
        let Some(pin) = nfo.ids.pin() else {
            return Ok(());
        };
        let describes = |wanted: NfoKind| nfo.kind.is_none_or(|kind| kind == wanted);
        let root = library.root_path.as_path();
        let is_this = |located: Option<LocatedNfo>| located.is_some_and(|l| l.path == nfo_path);

        let mut targets: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
        if is_tvshow_nfo(nfo_path) {
            if !describes(NfoKind::TvShow) {
                return Ok(());
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
                    show.pin_source,
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
                movie.pin_source,
                carried,
                &pin,
                PinConflict::Replace,
                nfo_path,
            )
            .await?;
        }
        Ok(())
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
    /// read is neither applied nor recorded, so it is tried again.
    pub(super) async fn reapply_nfo(
        &self,
        library: &Library,
        path: &Path,
        stored: Option<&AppliedNfo>,
        files: &[&MediaFile],
    ) -> Result<(), IndexError> {
        let Some(NfoRead { content, nfo }) = read_nfo_file(path) else {
            return Ok(());
        };
        if !stored.is_some_and(|stored| content.same_as(stored))
            && let Some(nfo) = &nfo
        {
            self.repin_from_nfo(library, path, nfo, files).await?;
        }
        if let Some(repo) = &self.applied_nfo_repo {
            let record = content.record(library.id, path, self.clock.now());
            if !stored.is_some_and(|stored| record.matches(stored)) {
                repo.record_by_path(record).await?;
            }
        }
        Ok(())
    }
}
