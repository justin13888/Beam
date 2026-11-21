//! [`InMemoryShowRepository`], the double for [`super::ShowRepository`], out
//! of the trait's file for size.

use super::*;
use crate::models::file::MediaFileContent;
use crate::models::watch_state::WatchTarget;
use crate::repositories::WatchStateRepository;
use crate::repositories::file::in_memory::InMemoryFileRepository;
use crate::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use crate::utils::media_path::EpisodeNumbering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The in-memory double. Linked to a file double with
/// [`InMemoryShowRepository::with_files`] it answers liveness and orphan
/// checks from those files; the unlinked `Default` treats every show as
/// live, exactly as `InMemoryMovieRepository` does.
#[derive(Debug, Default)]
pub struct InMemoryShowRepository {
    pub shows: Mutex<HashMap<Uuid, Show>>,
    pub seasons: Mutex<HashMap<Uuid, Season>>,
    pub episodes: Mutex<HashMap<Uuid, Episode>>,
    /// The rules version behind each show's key; absent is `0`, as for
    /// `InMemoryMovieRepository::key_versions`.
    pub key_versions: Mutex<HashMap<Uuid, u16>>,
    /// `(library_id, show_id)` associations.
    library_links: Mutex<HashSet<(Uuid, Uuid)>>,
    files: Option<Arc<InMemoryFileRepository>>,
    /// Where [`ShowRepository::rehome_episode`] carries a merged
    /// episode's watch state, when linked.
    watch_state: Option<Arc<InMemoryWatchStateRepository>>,
}

impl InMemoryShowRepository {
    /// A double whose liveness and orphan checks read `files`.
    pub fn with_files(files: Arc<InMemoryFileRepository>) -> Self {
        Self {
            files: Some(files),
            ..Self::default()
        }
    }

    /// This double, carrying watch state through `watch_state` when an
    /// episode is re-homed onto another.
    pub fn linking_watch_state(mut self, watch_state: Arc<InMemoryWatchStateRepository>) -> Self {
        self.watch_state = Some(watch_state);
        self
    }

    /// The oldest episode of `show_id` an absolutely numbered file
    /// indexed into season `indexed_season` gave the running number
    /// `absolute`, wherever it now sits.
    fn find_absolute(&self, show_id: Uuid, indexed_season: u32, absolute: u32) -> Option<Episode> {
        // `episodes` before `seasons`, as `child_counts` takes them.
        let episodes = self.episodes.lock().unwrap();
        let seasons = self.seasons.lock().unwrap();
        episodes
            .values()
            .filter(|e| e.absolute_number == Some(absolute))
            .filter(|e| e.absolute_season_number == Some(indexed_season))
            .filter(|e| seasons.get(&e.season_id).map(|s| s.show_id) == Some(show_id))
            .min_by_key(|e| (e.created_at, e.id))
            .cloned()
    }

    /// Every show associated with the library `library_id`, in no
    /// particular order: what a real store's `library_shows` join
    /// reads, for the doubles that stand in for one.
    pub fn ids_in_library(&self, library_id: Uuid) -> Vec<Uuid> {
        self.library_links
            .lock()
            .unwrap()
            .iter()
            .filter(|(library, _)| *library == library_id)
            .map(|(_, show)| *show)
            .collect()
    }

    /// Every library the show `show_id` is associated with, as
    /// `InMemoryMovieRepository::libraries_of`.
    pub fn libraries_of(&self, show_id: Uuid) -> BTreeSet<Uuid> {
        self.library_links
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, show)| *show == show_id)
            .map(|(library, _)| *library)
            .collect()
    }

    /// Episode ids some file row references -- only present files when
    /// `present_only`. `None` when unlinked.
    fn referenced_episodes(&self, present_only: bool) -> Option<HashSet<Uuid>> {
        let files = self.files.as_ref()?;
        Some(
            files
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| !present_only || f.missing_since.is_none())
                .filter_map(|f| match &f.content {
                    Some(MediaFileContent::Episode { episode_id, .. }) => Some(*episode_id),
                    _ => None,
                })
                .collect(),
        )
    }

    /// Show ids with an episode that has a present file, or `None` when
    /// unlinked.
    pub fn live_show_ids(&self) -> Option<HashSet<Uuid>> {
        let live_episodes = self.referenced_episodes(true)?;
        let episodes = self.episodes.lock().unwrap();
        let seasons = self.seasons.lock().unwrap();
        Some(
            live_episodes
                .iter()
                .filter_map(|id| episodes.get(id))
                .filter_map(|e| seasons.get(&e.season_id).map(|s| s.show_id))
                .collect(),
        )
    }
}

#[async_trait]
impl ShowRepository for InMemoryShowRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr> {
        Ok(self.shows.lock().unwrap().get(&id).cloned())
    }

    async fn find_all(&self) -> Result<Vec<Show>, DbErr> {
        Ok(self.shows.lock().unwrap().values().cloned().collect())
    }

    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Show>, DbErr> {
        let shows = self.shows.lock().unwrap();
        Ok(ids.iter().filter_map(|id| shows.get(id).cloned()).collect())
    }

    async fn child_counts(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, ShowChildCounts>, DbErr> {
        // `episodes` before `seasons`, the order `delete_orphaned` and
        // `live_show_ids` take them in.
        let episodes = self.episodes.lock().unwrap();
        let seasons = self.seasons.lock().unwrap();
        let mut counts: HashMap<Uuid, ShowChildCounts> = HashMap::new();
        for season in seasons.values().filter(|s| show_ids.contains(&s.show_id)) {
            let entry = counts.entry(season.show_id).or_default();
            entry.seasons += 1;
            entry.episodes += episodes
                .values()
                .filter(|e| e.season_id == season.id)
                .count() as u32;
        }
        Ok(counts)
    }

    async fn find_or_create_by_identity(&self, create: CreateShow) -> Result<Show, DbErr> {
        let CreateShow {
            identity_key,
            identity_key_version,
            title,
            year,
        } = create;
        // Lookup and insert under one lock, as atomic as `ON CONFLICT`.
        let mut shows = self.shows.lock().unwrap();
        if let Some(existing) = shows
            .values()
            .find(|s| s.identity_key.as_deref() == Some(identity_key.as_str()))
        {
            return Ok(existing.clone());
        }
        let show = Show {
            id: Uuid::new_v4(),
            title,
            identity_key: Some(identity_key),
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year,
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
        };
        shows.insert(show.id, show.clone());
        self.key_versions
            .lock()
            .unwrap()
            .insert(show.id, identity_key_version);
        Ok(show)
    }

    async fn find_unkeyed(&self) -> Result<Vec<Show>, DbErr> {
        let mut unkeyed: Vec<_> = self
            .shows
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.identity_key.is_none())
            .cloned()
            .collect();
        unkeyed.sort_by_key(|s| (s.created_at, s.id));
        Ok(unkeyed)
    }

    async fn assign_identity_key(
        &self,
        show_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr> {
        let mut shows = self.shows.lock().unwrap();
        if shows
            .values()
            .any(|s| s.identity_key.as_deref() == Some(identity_key))
        {
            return Ok(false);
        }
        match shows.get_mut(&show_id) {
            Some(show) if show.identity_key.is_none() => {
                show.identity_key = Some(identity_key.to_string());
                self.key_versions.lock().unwrap().insert(show_id, version);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Show>, DbErr> {
        Ok(self
            .shows
            .lock()
            .unwrap()
            .values()
            .find(|s| s.identity_key.as_deref() == Some(identity_key))
            .cloned())
    }

    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Show>, DbErr> {
        // `shows` before `key_versions`, the order every method takes
        // them in, so no two calls can deadlock.
        let shows = self.shows.lock().unwrap();
        let versions = self.key_versions.lock().unwrap();
        let mut stale: Vec<_> = shows
            .values()
            .filter(|s| s.identity_key.is_some())
            .filter(|s| versions.get(&s.id).copied().unwrap_or(0) < version)
            .cloned()
            .collect();
        stale.sort_by_key(|s| (s.created_at, s.id));
        Ok(stale)
    }

    async fn rekey(
        &self,
        show_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr> {
        let mut shows = self.shows.lock().unwrap();
        if let Some(key) = identity_key.as_deref()
            && shows
                .values()
                .any(|s| s.id != show_id && s.identity_key.as_deref() == Some(key))
        {
            return Ok(false);
        }
        let Some(show) = shows.get_mut(&show_id) else {
            return Ok(false);
        };
        show.identity_key = identity_key;
        self.key_versions.lock().unwrap().insert(show_id, version);
        Ok(true)
    }

    async fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Show>, DbErr> {
        let stored = pin.to_ref_string();
        let shows = self.shows.lock().unwrap();
        if let Some(pinned) = shows
            .values()
            .find(|t| t.pinned_ref.as_deref() == Some(stored.as_str()))
        {
            return Ok(Some(pinned.clone()));
        }
        let matched = |t: &Show| match pin {
            ProviderPin::Tmdb(id) => t.tmdb_id == Some(*id),
            ProviderPin::Imdb(id) => t.imdb_id.as_deref() == Some(id.as_str()),
            ProviderPin::Tvdb(id) => t.tvdb_id == Some(*id),
            ProviderPin::Anilist(id) => t.anilist_id == Some(*id),
        };
        Ok(shows
            .values()
            .filter(|t| matched(t))
            .min_by_key(|t| (t.created_at, t.id))
            .cloned())
    }

    async fn set_pinned_ref(
        &self,
        show_id: Uuid,
        pin: &ProviderPin,
        source: PinSource,
    ) -> Result<bool, DbErr> {
        let stored = pin.to_ref_string();
        let mut shows = self.shows.lock().unwrap();
        if shows
            .values()
            .any(|t| t.id != show_id && t.pinned_ref.as_deref() == Some(stored.as_str()))
        {
            return Ok(false);
        }
        match shows.get_mut(&show_id) {
            Some(title)
                if source == PinSource::Nfo && title.pin_source == Some(PinSource::Admin) =>
            {
                Ok(false)
            }
            Some(title) => {
                title.pinned_ref = Some(stored);
                title.pin_source = Some(source);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn clear_admin_pin(&self, show_id: Uuid) -> Result<bool, DbErr> {
        let mut shows = self.shows.lock().unwrap();
        match shows.get_mut(&show_id) {
            Some(title) if title.pin_source == Some(PinSource::Admin) => {
                title.pinned_ref = None;
                title.pin_source = None;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr> {
        let Some(referenced) = self.referenced_episodes(false) else {
            return Ok(0);
        };
        let mut episodes = self.episodes.lock().unwrap();
        episodes.retain(|id, e| e.created_at >= created_before || referenced.contains(id));
        let mut seasons = self.seasons.lock().unwrap();
        seasons.retain(|id, _| episodes.values().any(|e| e.season_id == *id));
        let mut shows = self.shows.lock().unwrap();
        let before = shows.len();
        shows.retain(|id, s| {
            s.created_at >= created_before || seasons.values().any(|se| se.show_id == *id)
        });
        Ok((before - shows.len()) as u64)
    }

    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        show_id: Uuid,
    ) -> Result<(), DbErr> {
        self.library_links
            .lock()
            .unwrap()
            .insert((library_id, show_id));
        Ok(())
    }

    async fn library_ids(&self, show_ids: &[Uuid]) -> Result<HashMap<Uuid, BTreeSet<Uuid>>, DbErr> {
        let mut libraries: HashMap<Uuid, BTreeSet<Uuid>> = HashMap::new();
        for (library_id, show_id) in self.library_links.lock().unwrap().iter() {
            if show_ids.contains(show_id) {
                libraries.entry(*show_id).or_default().insert(*library_id);
            }
        }
        Ok(libraries)
    }

    async fn find_or_create_season(
        &self,
        show_id: Uuid,
        season_number: u32,
    ) -> Result<Season, DbErr> {
        // One lock for the lookup and the insert, as the Postgres
        // `ON CONFLICT` is one statement.
        let mut seasons = self.seasons.lock().unwrap();
        if let Some(s) = seasons
            .values()
            .find(|s| s.show_id == show_id && s.season_number == season_number)
        {
            return Ok(s.clone());
        }
        let season = Season {
            id: Uuid::new_v4(),
            show_id,
            season_number,
            poster_url: None,
            first_aired: None,
            last_aired: None,
        };
        seasons.insert(season.id, season.clone());
        Ok(season)
    }

    async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr> {
        let mut seasons: Vec<Season> = self
            .seasons
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.show_id == show_id)
            .cloned()
            .collect();
        seasons.sort_by_key(|s| s.season_number);
        Ok(seasons)
    }

    async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr> {
        let mut episodes: Vec<Episode> = self
            .episodes
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.season_id == season_id)
            .cloned()
            .collect();
        episodes.sort_by_key(|e| e.episode_number);
        Ok(episodes)
    }

    async fn find_or_create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr> {
        let CreateEpisode {
            season_id,
            episode_number,
            title,
            runtime,
            air_date,
            numbering,
        } = create;
        let season = self
            .seasons
            .lock()
            .unwrap()
            .get(&season_id)
            .map(|s| (s.show_id, s.season_number));
        // The running number and the season it was read in.
        let absolute_place = match (numbering, season) {
            (EpisodeNumbering::Absolute, Some((_, season_number))) => {
                Some((episode_number, season_number))
            }
            _ => None,
        };
        if let (Some((absolute, indexed_season)), Some((show_id, _))) = (absolute_place, season)
            && let Some(rehomed) = self.find_absolute(show_id, indexed_season, absolute)
        {
            return Ok(rehomed);
        }
        // Lookup and insert under one lock, so the double is as atomic as
        // the `ON CONFLICT` statement it stands in for.
        let mut episodes = self.episodes.lock().unwrap();
        if let Some(existing) = episodes
            .values_mut()
            .find(|e| e.season_id == season_id && e.episode_number == episode_number)
        {
            // A standard row with no running number, met by an
            // absolutely numbered file, is one indexed before numbering
            // was recorded: it takes the file's numbering.
            if let Some((absolute, indexed_season)) = absolute_place
                && existing.numbering == EpisodeNumbering::Standard
                && existing.absolute_number.is_none()
            {
                existing.numbering = EpisodeNumbering::Absolute;
                existing.absolute_number = Some(absolute);
                existing.absolute_season_number = Some(indexed_season);
            }
            return Ok(existing.clone());
        }
        let ep = Episode {
            id: Uuid::new_v4(),
            season_id,
            episode_number,
            title,
            description: None,
            air_date,
            runtime,
            thumbnail_url: None,
            numbering,
            absolute_number: absolute_place.map(|(absolute, _)| absolute),
            absolute_season_number: absolute_place.map(|(_, indexed_season)| indexed_season),
            created_at: chrono::Utc::now(),
        };
        episodes.insert(ep.id, ep.clone());
        Ok(ep)
    }

    async fn rehome_episode(
        &self,
        episode_id: Uuid,
        season_number: u32,
        episode_number: u32,
    ) -> Result<Option<Episode>, DbErr> {
        let Some(source) = self.episodes.lock().unwrap().get(&episode_id).cloned() else {
            return Ok(None);
        };
        let Some(show_id) = self
            .seasons
            .lock()
            .unwrap()
            .get(&source.season_id)
            .map(|s| s.show_id)
        else {
            return Ok(None);
        };
        let season = self.find_or_create_season(show_id, season_number).await?;
        let occupant = self
            .episodes
            .lock()
            .unwrap()
            .values()
            .find(|e| e.season_id == season.id && e.episode_number == episode_number)
            .cloned();
        let home = match occupant {
            Some(occupant) if occupant.id == source.id => return Ok(Some(occupant)),
            Some(occupant) => {
                if let Some(files) = &self.files {
                    for file in files.files.lock().unwrap().values_mut() {
                        if let Some(MediaFileContent::Episode { episode_id, .. }) =
                            &mut file.content
                            && *episode_id == source.id
                        {
                            *episode_id = occupant.id;
                        }
                    }
                }
                if let Some(watch_state) = &self.watch_state {
                    let target = |episode_id| WatchTarget::Episode {
                        episode_id,
                        show_id,
                    };
                    watch_state
                        .carry(target(source.id), target(occupant.id))
                        .await?;
                }
                let mut episodes = self.episodes.lock().unwrap();
                episodes.remove(&source.id);
                let kept = episodes
                    .get_mut(&occupant.id)
                    .expect("the occupant was just read");
                // The running number and its season go together.
                if kept.absolute_number.is_none() {
                    kept.absolute_number = source.absolute_number;
                    kept.absolute_season_number = source.absolute_season_number;
                }
                kept.clone()
            }
            None => {
                let mut episodes = self.episodes.lock().unwrap();
                let moved = episodes
                    .get_mut(&source.id)
                    .expect("the source was just read");
                moved.season_id = season.id;
                moved.episode_number = episode_number;
                moved.clone()
            }
        };
        // A season the move empties goes with it.
        let emptied = !self
            .episodes
            .lock()
            .unwrap()
            .values()
            .any(|e| e.season_id == source.season_id);
        if emptied {
            self.seasons.lock().unwrap().remove(&source.season_id);
        }
        Ok(Some(home))
    }

    async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr> {
        Ok(self.episodes.lock().unwrap().get(&episode_id).cloned())
    }

    async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr> {
        Ok(self.seasons.lock().unwrap().get(&season_id).cloned())
    }

    async fn episode_outline(&self, show_id: Uuid) -> Result<Vec<OutlineEpisode>, DbErr> {
        // Unlinked, every episode is playable, as every show is live.
        let present = self.referenced_episodes(true);
        let episodes = self.episodes.lock().unwrap();
        let seasons = self.seasons.lock().unwrap();
        let mut outline: Vec<OutlineEpisode> = episodes
            .values()
            .filter_map(|episode| {
                let season = seasons.get(&episode.season_id)?;
                (season.show_id == show_id).then(|| OutlineEpisode {
                    episode_id: episode.id,
                    season_number: season.season_number,
                    episode_number: episode.episode_number,
                    playable: present
                        .as_ref()
                        .is_none_or(|present| present.contains(&episode.id)),
                })
            })
            .collect();
        outline.sort_by_key(|e| (e.season_number, e.episode_number));
        Ok(outline)
    }

    async fn episode_outlines(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<OutlineEpisode>>, DbErr> {
        let mut outlines = HashMap::new();
        for show_id in show_ids {
            let outline = self.episode_outline(*show_id).await?;
            if !outline.is_empty() {
                outlines.insert(*show_id, outline);
            }
        }
        Ok(outlines)
    }

    async fn apply_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &ShowEnrichment,
        locks: &FieldLocks,
    ) -> Result<(), DbErr> {
        let mut shows = self.shows.lock().unwrap();
        if let Some(show) = shows.get_mut(&show_id) {
            let open = |field| !locks.is_locked(field);
            if open(MetadataField::Title) {
                show.title = enrichment.title.clone();
            }
            if open(MetadataField::OriginalTitle) {
                show.title_localized = enrichment.original_title.clone();
            }
            if open(MetadataField::Description) {
                show.description = enrichment.description.clone();
            }
            if open(MetadataField::Year) {
                show.year = enrichment.year;
            }
            if open(MetadataField::Poster) {
                show.poster_url = enrichment.poster_url.clone();
            }
            if open(MetadataField::Backdrop) {
                show.backdrop_url = enrichment.backdrop_url.clone();
            }
            show.tmdb_id = enrichment.tmdb_id;
            show.imdb_id = enrichment.imdb_id.clone();
            show.anilist_id = enrichment.anilist_id;
            show.tvdb_id = enrichment.tvdb_id;
            if open(MetadataField::Rating) {
                show.rating_tmdb = enrichment.rating;
            }
            show.content_rating = enrichment.content_rating.clone();
            show.updated_at = chrono::Utc::now();
        }
        Ok(())
    }

    async fn apply_season_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &SeasonEnrichment,
    ) -> Result<u32, DbErr> {
        let season_id = {
            let mut seasons = self.seasons.lock().unwrap();
            let Some(season) = seasons
                .values_mut()
                .find(|s| s.show_id == show_id && s.season_number == enrichment.season_number)
            else {
                return Ok(0);
            };
            season.poster_url = enrichment.poster_url.clone();
            season.first_aired = enrichment.air_date;
            season.id
        };

        let mut episodes = self.episodes.lock().unwrap();
        let mut updated = 0u32;
        for ep_enrichment in &enrichment.episodes {
            if let Some(episode) = episodes.values_mut().find(|e| {
                e.season_id == season_id && e.episode_number == ep_enrichment.episode_number
            }) {
                if let Some(title) = &ep_enrichment.title {
                    episode.title = title.clone();
                }
                if ep_enrichment.description.is_some() {
                    episode.description = ep_enrichment.description.clone();
                }
                if let Some(air_date) = ep_enrichment.air_date {
                    episode.air_date = Some(air_date);
                }
                if let Some(runtime_mins) = ep_enrichment.runtime_mins {
                    episode.runtime =
                        Some(std::time::Duration::from_secs((runtime_mins as u64) * 60));
                }
                if ep_enrichment.thumbnail_url.is_some() {
                    episode.thumbnail_url = ep_enrichment.thumbnail_url.clone();
                }
                updated += 1;
            }
        }
        Ok(updated)
    }
}
