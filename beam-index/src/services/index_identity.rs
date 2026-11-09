//! The identity passes: backfilling and rekeying the identity keys movies
//! and shows are matched by, and carrying admin state across a merge.

use super::*;

/// What [`LocalIndexService::backfill_identity_keys`] did: how many legacy
/// titles it keyed, and which it had to leave keyless and why.
#[derive(Debug, Default)]
pub(super) struct IdentityBackfill {
    pub(super) keyed: u64,
    /// Titles whose files derive more than one key.
    pub(super) ambiguous_movies: Vec<Uuid>,
    pub(super) ambiguous_shows: Vec<Uuid>,
    /// Titles whose key another title already holds.
    pub(super) clashing_movies: Vec<Uuid>,
    pub(super) clashing_shows: Vec<Uuid>,
}

/// What [`LocalIndexService::rekey_stale_titles`] did.
#[derive(Debug, Default)]
pub(super) struct IdentityRekey {
    /// Titles whose key changed in place.
    pub(super) rekeyed: u64,
    /// `(kept, retired)`: titles merged because the current rules read them
    /// as one.
    pub(super) merged_movies: Vec<(Uuid, Uuid)>,
    pub(super) merged_shows: Vec<(Uuid, Uuid)>,
    /// Titles whose files derive more than one key, which keep their old one.
    pub(super) ambiguous_movies: Vec<Uuid>,
    pub(super) ambiguous_shows: Vec<Uuid>,
    /// `(stale, holder)`: titles the current rules read as one, left apart
    /// because providers matched them to different entries (see
    /// [`provider_ids_conflict`]). Each keeps its key and its files.
    pub(super) conflicting_movies: Vec<(Uuid, Uuid)>,
    pub(super) conflicting_shows: Vec<(Uuid, Uuid)>,
}

/// The one key `rows` derive through `key_of`: from the present files when
/// any derives one, else from every row. `None` when they derive none, or
/// more than one.
pub(super) fn derived_key(
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
pub(super) fn has_provider_ids(
    tmdb_id: Option<u32>,
    imdb_id: &Option<String>,
    tvdb_id: Option<u32>,
    anilist_id: Option<u32>,
) -> bool {
    tmdb_id.is_some() || imdb_id.is_some() || tvdb_id.is_some() || anilist_id.is_some()
}

/// A title's provider ids, as [`provider_ids_conflict`] compares them.
#[derive(Debug, Clone, Copy)]
pub(super) struct ProviderIds<'a> {
    pub(super) tmdb: Option<u32>,
    pub(super) imdb: Option<&'a str>,
    pub(super) tvdb: Option<u32>,
    pub(super) anilist: Option<u32>,
}

impl<'a> ProviderIds<'a> {
    pub(super) fn of_movie(movie: &'a Movie) -> Self {
        Self {
            tmdb: movie.tmdb_id,
            imdb: movie.imdb_id.as_deref(),
            tvdb: movie.tvdb_id,
            anilist: movie.anilist_id,
        }
    }

    pub(super) fn of_show(show: &'a Show) -> Self {
        Self {
            tmdb: show.tmdb_id,
            imdb: show.imdb_id.as_deref(),
            tvdb: show.tvdb_id,
            anilist: show.anilist_id,
        }
    }

    pub(super) fn any(&self) -> bool {
        self.tmdb.is_some() || self.imdb.is_some() || self.tvdb.is_some() || self.anilist.is_some()
    }
}

/// Whether two titles are matched to different provider entries: both carry
/// provider ids, and they disagree on one both carry or share none to agree
/// on. Merging such a pair would discard one match, and matches that
/// disagree say the titles are two, whatever the naming rules read -- `The
/// Godfather Part 2` and `The Godfather` are two films however a rule
/// misreads their names (C3 of the #233 review).
pub(super) fn provider_ids_conflict(a: ProviderIds<'_>, b: ProviderIds<'_>) -> bool {
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
pub(super) fn survives(
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
///
/// A disc structure's stream file keeps its stored part: its part is its
/// place in the disc's main title, which its name does not say (issue #234),
/// and a rekey reads no disc.
pub(super) fn part_as_read(file: &MediaFile, inferred: &MediaInference, key: &str) -> Option<u32> {
    match inferred {
        MediaInference::Movie(inferred)
            if inferred.title.identity_key() == key && disc_stream_kind(&file.path).is_none() =>
        {
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
pub(super) fn movie_key(inference: &MediaInference) -> Option<String> {
    match inference {
        MediaInference::Movie(movie) => Some(movie.title.identity_key()),
        MediaInference::Episode(_) | MediaInference::Unclassifiable(_) => None,
    }
}

/// The identity key of the show a file's path names, if it is an episode.
pub(super) fn show_key(inference: &MediaInference) -> Option<String> {
    match inference {
        MediaInference::Episode(episode) => Some(episode.series.identity_key()),
        MediaInference::Movie(_) | MediaInference::Unclassifiable(_) => None,
    }
}

/// Whether a show identity key names a season folder (`season 05|`): the
/// key of a husk, the show a build before issue #182 named after the season
/// folder its episodes sat in. Read from a key, never from the display
/// title, which enrichment rewrites.
pub(super) fn is_season_folder_husk_key(key: &str) -> bool {
    let title = key.split_once('|').map_or(key, |(title, _)| title);
    season_folder_number(title).is_some()
}

/// Whether `file` was classified by older rules and is to be reclassified:
/// a probed row below [`CLASSIFIER_VERSION`]. An unprobed row -- its probe
/// failed -- was never classified, so there is nothing to reclassify.
pub(super) fn awaits_reclassification(file: &MediaFile) -> bool {
    let probed = file.content.is_some() || file.duration.is_some();
    file.classifier_version < CLASSIFIER_VERSION && probed
}

/// The key a build before issue #182 derived for the show of the episode at
/// `path`: its parent folder's parse, whatever that folder was.
pub(super) fn legacy_show_key(path: &Path) -> String {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let ParsedFilename { title, year, .. } = parse_media_filename(&folder);
    title_identity_key(&title, year)
}

impl LocalIndexService {
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
    pub(super) async fn backfill_identity_keys(&self) -> Result<IdentityBackfill, IndexError> {
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
    pub(super) async fn carry_admin_state(
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
    pub(super) async fn rekey_stale_titles(&self) -> Result<IdentityRekey, IndexError> {
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
            // The retired movie's viewers keep their place on the survivor;
            // left behind, it would go when the retired movie does. Carried
            // before any file moves, so a pass that stops part-way leaves the
            // state on the title that keeps it -- never on one the move has
            // emptied for orphan cleanup to retire (issue #246).
            self.progress_repo
                .carry(
                    WatchTarget::Movie { movie_id: loser },
                    WatchTarget::Movie { movie_id: survivor },
                )
                .await?;
            // So are their watchlists and favourites, for the same reason.
            self.carry_list_entries(TitleRef::Movie(loser), TitleRef::Movie(survivor))
                .await?;
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
            // Users' lists hold the show itself, not its episodes: carried
            // once, before anything moves, as a movie merge carries them.
            self.carry_list_entries(TitleRef::Show(loser), TitleRef::Show(survivor))
                .await?;
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
                            air_date,
                        })
                        .await?;
                    // Carried before the episode's files move, as for a
                    // movie merge.
                    self.progress_repo
                        .carry(
                            WatchTarget::Episode {
                                episode_id,
                                show_id: loser,
                            },
                            WatchTarget::Episode {
                                episode_id: target.id,
                                show_id: survivor,
                            },
                        )
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
    pub(super) async fn hold_classification(
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
    pub(super) async fn read_held_parts(
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
    pub(super) async fn retitle_as_spelled(
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

    /// The title watch state is kept against for a file of `content`: its
    /// movie, or its episode and that episode's show. `None` when the entry
    /// or episode is gone.
    pub(super) async fn watch_target_of(
        &self,
        content: &MediaFileContent,
    ) -> Result<Option<WatchTarget>, DbErr> {
        match *content {
            MediaFileContent::Movie { movie_entry_id, .. } => Ok(self
                .movie_repo
                .find_entry_by_id(movie_entry_id)
                .await?
                .map(|entry| WatchTarget::Movie {
                    movie_id: entry.movie_id,
                })),
            MediaFileContent::Episode { episode_id, .. } => {
                let Some(episode) = self.show_repo.find_episode_by_id(episode_id).await? else {
                    return Ok(None);
                };
                Ok(self
                    .show_repo
                    .find_season_by_id(episode.season_id)
                    .await?
                    .map(|season| WatchTarget::Episode {
                        episode_id,
                        show_id: season.show_id,
                    }))
            }
        }
    }

    /// Before the file `file_id` moves from `from` to `to`, carry the watch
    /// state of the title it leaves onto the one it joins -- when that move
    /// leaves the title with no other visible file, so that orphan cleanup
    /// would otherwise retire it with its viewers' state (issue #246). A
    /// title another file still holds keeps its state; so does one moving to
    /// a title of the other kind, which no watch state can follow.
    ///
    /// Users' watchlist and favourites entries are carried the same way
    /// (issue #262): a movie's with its watch state, and a show's when the
    /// move leaves the show itself -- not only the episode -- with no other
    /// file, since lists hold the show rather than its episodes.
    pub(super) async fn carry_from_emptied_title(
        &self,
        file_id: Uuid,
        from: &MediaFileContent,
        to: &MediaFileContent,
    ) -> Result<(), DbErr> {
        let (Some(from), Some(to)) = (
            self.watch_target_of(from).await?,
            self.watch_target_of(to).await?,
        ) else {
            return Ok(());
        };
        let keeps_a_file = match (from, to) {
            (WatchTarget::Movie { movie_id }, WatchTarget::Movie { movie_id: to_movie }) => {
                if movie_id == to_movie {
                    return Ok(());
                }
                let mut keeps = false;
                for entry in self.movie_repo.find_entries_by_movie_id(movie_id).await? {
                    if self
                        .file_repo
                        .find_by_movie_entry_id(entry.id)
                        .await?
                        .iter()
                        .any(|file| file.id != file_id)
                    {
                        keeps = true;
                        break;
                    }
                }
                keeps
            }
            (
                WatchTarget::Episode { episode_id, .. },
                WatchTarget::Episode {
                    episode_id: to_episode,
                    ..
                },
            ) => {
                if episode_id == to_episode {
                    return Ok(());
                }
                self.file_repo
                    .find_by_episode_id(episode_id)
                    .await?
                    .iter()
                    .any(|file| file.id != file_id)
            }
            (WatchTarget::Movie { .. }, WatchTarget::Episode { .. })
            | (WatchTarget::Episode { .. }, WatchTarget::Movie { .. }) => return Ok(()),
        };
        if keeps_a_file {
            return Ok(());
        }
        self.progress_repo.carry(from, to).await?;
        match (from, to) {
            (WatchTarget::Movie { movie_id }, WatchTarget::Movie { movie_id: to_movie }) => {
                self.carry_list_entries(TitleRef::Movie(movie_id), TitleRef::Movie(to_movie))
                    .await
            }
            (
                WatchTarget::Episode { show_id, .. },
                WatchTarget::Episode {
                    show_id: to_show, ..
                },
            ) => {
                if show_id == to_show || self.show_keeps_a_file(show_id, file_id).await? {
                    return Ok(());
                }
                self.carry_list_entries(TitleRef::Show(show_id), TitleRef::Show(to_show))
                    .await
            }
            (WatchTarget::Movie { .. }, WatchTarget::Episode { .. })
            | (WatchTarget::Episode { .. }, WatchTarget::Movie { .. }) => Ok(()),
        }
    }

    /// Whether any episode of `show_id` holds a file other than `file_id`.
    pub(super) async fn show_keeps_a_file(
        &self,
        show_id: Uuid,
        file_id: Uuid,
    ) -> Result<bool, DbErr> {
        for season in self.show_repo.find_seasons_by_show_id(show_id).await? {
            for episode in self.show_repo.find_episodes_by_season_id(season.id).await? {
                if self
                    .file_repo
                    .find_by_episode_id(episode.id)
                    .await?
                    .iter()
                    .any(|file| file.id != file_id)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Point `file` at `content`, keeping its status and classifier version.
    pub(super) async fn move_file(
        &self,
        file: &MediaFile,
        content: MediaFileContent,
    ) -> Result<(), DbErr> {
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
    pub(super) async fn identity_passes_done(&self) -> bool {
        if self.identity_passes_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let exclusive = self.scans.exclusive_catalog().await;
        self.run_identity_passes(exclusive).await
    }

    /// [`Self::identity_passes_done`] for a watcher event, which never waits:
    /// `false` when a scan or reconcile holds the catalog gate. The file then
    /// keeps its classification until a later visit.
    pub(super) async fn identity_passes_done_now(&self) -> bool {
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
    pub(super) async fn run_identity_passes(&self, _exclusive: CatalogExclusive) -> bool {
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
}
