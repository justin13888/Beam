use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::pin::ProviderPin;
use crate::models::show::{CreateEpisode, CreateShow, Episode, Season, Show, ShowSearchQuery};
use crate::providers::enrichment::{SeasonEnrichment, ShowEnrichment};

/// Persistence for shows, their seasons and their episodes.
///
/// A show is keyed and kept live exactly as a movie is -- see
/// [`crate::repositories::MovieRepository`]: the indexer matches a series
/// folder to a show by its identity key, never its display title, and a show
/// is live while at least one episode has a present file.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    /// Live or not.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
    /// Every show, live or not.
    async fn find_all(&self) -> Result<Vec<Show>, DbErr>;
    /// Server-side filtered/ranked search, mirroring
    /// `MovieRepository::search`: only live shows.
    async fn search(&self, query: &ShowSearchQuery) -> Result<Vec<Show>, DbErr>;
    /// The show keyed `create.identity_key`, inserting it only if no show
    /// carries that key yet. An existing show is returned unchanged; a show
    /// with no key is never matched.
    ///
    /// Atomic: concurrent calls for one key all return the same row. This is
    /// what lets two episodes of a new show be indexed at once without the
    /// second failing on, or duplicating, the first's show.
    async fn find_or_create_by_identity(&self, create: CreateShow) -> Result<Show, DbErr>;
    /// Every show with no identity key, for the indexer's backfill, oldest
    /// first (`created_at`, then `id`), so of two legacy duplicates the
    /// backfill keys the original.
    async fn find_unkeyed(&self) -> Result<Vec<Show>, DbErr>;
    /// Give the keyless show `show_id` the key `identity_key`, derived by
    /// version `version` of the rules. Returns `false`, changing nothing, when
    /// the show does not exist, already has a key, or another show already
    /// holds `identity_key`.
    async fn assign_identity_key(
        &self,
        show_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// The show keyed `identity_key`, if any.
    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Show>, DbErr>;
    /// Every keyed show whose key an older version of the rules than
    /// `version` derived, oldest first -- see
    /// [`crate::repositories::MovieRepository::find_keyed_before_version`].
    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Show>, DbErr>;
    /// Replace the key of `show_id` with `identity_key` (`None`: keyless),
    /// derived by version `version` of the rules, exactly as
    /// [`crate::repositories::MovieRepository::rekey`] does for a movie.
    async fn rekey(
        &self,
        show_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// The show `pin` names (issue #184): the one pinned to it, else the
    /// oldest one enrichment matched to that provider id (its `tmdb_id`,
    /// `imdb_id`, `tvdb_id` or `anilist_id`). A file whose NFO names `pin`
    /// joins this show whatever its path is keyed as.
    async fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Show>, DbErr>;
    /// Pin `show_id` to `pin`, replacing any pin it had. Returns `false`,
    /// changing nothing, when the show does not exist or another show
    /// is already pinned to `pin`.
    async fn set_pinned_ref(&self, show_id: Uuid, pin: &ProviderPin) -> Result<bool, DbErr>;
    /// Delete every episode created before `created_before` that no file row
    /// references, then every season left with no episode, then every show
    /// created before `created_before` left with no season, returning how many
    /// shows went. As for movies, a soft-deleted file row still counts.
    /// Deleting a show takes its library associations, enrichment state and
    /// genre links with it.
    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr>;
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        show_id: Uuid,
    ) -> Result<(), DbErr>;
    async fn find_or_create_season(
        &self,
        show_id: Uuid,
        season_number: u32,
    ) -> Result<Season, DbErr>;
    async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr>;
    async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr>;
    /// The episode numbered `create.episode_number` in `create.season_id`,
    /// inserting it only if no such episode exists yet.
    ///
    /// An episode is one logical row per `(season_id, episode_number)` -- the
    /// pair carries a unique index -- however many files are indexed for it: a
    /// 1080p and a 720p rip of the same episode are two sources of one
    /// episode. When the episode already exists it is returned **unchanged**:
    /// `create.title` and `create.runtime` are used only to populate a new row,
    /// so a later file's filename parse never overwrites what the first file
    /// (or enrichment since) established. Mirrors the movie side, where the
    /// indexer reuses a movie found by identity key without touching it.
    ///
    /// Atomic: concurrent calls for one pair all return the same row.
    async fn find_or_create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr>;
    /// Reverse lookup from a `MediaFileContent::Episode { episode_id, .. }` back
    /// to the episode -- used together with `find_season_by_id` to resolve a
    /// file id to its show for continue-watching.
    async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr>;
    /// Reverse lookup from `Episode::season_id` to the season (and, via
    /// `Season::show_id`, the show).
    async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr>;
    /// Apply enrichment-provider data to an existing show. Overwrites the
    /// current values, same as `MovieRepository::apply_enrichment`, and
    /// likewise never touches the identity key.
    async fn apply_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &ShowEnrichment,
    ) -> Result<(), DbErr>;
    /// Apply a season's enrichment to the show's *existing* season/episode
    /// rows. Never fabricates a season or episode that scanning hasn't
    /// already created from a real file -- rows with no local counterpart
    /// are silently skipped. Returns the number of episodes updated.
    async fn apply_season_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &SeasonEnrichment,
    ) -> Result<u32, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::MediaFileContent;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
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
        files: Option<Arc<InMemoryFileRepository>>,
    }

    impl InMemoryShowRepository {
        /// A double whose liveness and orphan checks read `files`.
        pub fn with_files(files: Arc<InMemoryFileRepository>) -> Self {
            Self {
                files: Some(files),
                ..Self::default()
            }
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
        fn live_shows(&self) -> Option<HashSet<Uuid>> {
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

        async fn search(&self, query: &ShowSearchQuery) -> Result<Vec<Show>, DbErr> {
            use crate::models::search::title_match_score;

            let live = self.live_shows();
            let mut scored: Vec<(f64, Show)> = self
                .shows
                .lock()
                .unwrap()
                .values()
                .filter(|s| live.as_ref().is_none_or(|live| live.contains(&s.id)))
                .filter(|s| {
                    if query.year.is_some_and(|y| s.year != Some(y)) {
                        return false;
                    }
                    if query.year_from.is_some_and(|yf| s.year.unwrap_or(0) < yf) {
                        return false;
                    }
                    if query
                        .year_to
                        .is_some_and(|yt| s.year.unwrap_or(u32::MAX) > yt)
                    {
                        return false;
                    }
                    true
                })
                .filter_map(|s| {
                    let score = match &query.query {
                        Some(q) => title_match_score(&s.title, q),
                        None => 1.0,
                    };
                    (score > 0.0).then(|| (score, s.clone()))
                })
                .collect();

            scored.sort_by(|(a_score, a), (b_score, b)| {
                b_score
                    .partial_cmp(a_score)
                    .unwrap()
                    .then_with(|| a.title.cmp(&b.title))
            });
            Ok(scored.into_iter().map(|(_, s)| s).collect())
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
                title_localized: None,
                description: None,
                year,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
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

        async fn set_pinned_ref(&self, show_id: Uuid, pin: &ProviderPin) -> Result<bool, DbErr> {
            let stored = pin.to_ref_string();
            let mut shows = self.shows.lock().unwrap();
            if shows
                .values()
                .any(|t| t.id != show_id && t.pinned_ref.as_deref() == Some(stored.as_str()))
            {
                return Ok(false);
            }
            match shows.get_mut(&show_id) {
                Some(title) => {
                    title.pinned_ref = Some(stored);
                    Ok(true)
                }
                None => Ok(false),
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
            _library_id: Uuid,
            _show_id: Uuid,
        ) -> Result<(), DbErr> {
            Ok(())
        }

        async fn find_or_create_season(
            &self,
            show_id: Uuid,
            season_number: u32,
        ) -> Result<Season, DbErr> {
            {
                let guard = self.seasons.lock().unwrap();
                if let Some(s) = guard
                    .values()
                    .find(|s| s.show_id == show_id && s.season_number == season_number)
                {
                    return Ok(s.clone());
                }
            }
            let season = Season {
                id: Uuid::new_v4(),
                show_id,
                season_number,
                poster_url: None,
                first_aired: None,
                last_aired: None,
            };
            self.seasons
                .lock()
                .unwrap()
                .insert(season.id, season.clone());
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
            } = create;
            // Lookup and insert under one lock, so the double is as atomic as
            // the `ON CONFLICT` statement it stands in for.
            let mut episodes = self.episodes.lock().unwrap();
            if let Some(existing) = episodes
                .values()
                .find(|e| e.season_id == season_id && e.episode_number == episode_number)
            {
                return Ok(existing.clone());
            }
            let ep = Episode {
                id: Uuid::new_v4(),
                season_id,
                episode_number,
                title,
                description: None,
                air_date: air_date.map(|d| d.to_string()),
                runtime,
                thumbnail_url: None,
                created_at: chrono::Utc::now(),
            };
            episodes.insert(ep.id, ep.clone());
            Ok(ep)
        }

        async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr> {
            Ok(self.episodes.lock().unwrap().get(&episode_id).cloned())
        }

        async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr> {
            Ok(self.seasons.lock().unwrap().get(&season_id).cloned())
        }

        async fn apply_enrichment(
            &self,
            show_id: Uuid,
            enrichment: &ShowEnrichment,
        ) -> Result<(), DbErr> {
            let mut shows = self.shows.lock().unwrap();
            if let Some(show) = shows.get_mut(&show_id) {
                show.title = enrichment.title.clone();
                show.title_localized = enrichment.original_title.clone();
                show.description = enrichment.description.clone();
                show.year = enrichment.year;
                show.poster_url = enrichment.poster_url.clone();
                show.backdrop_url = enrichment.backdrop_url.clone();
                show.tmdb_id = enrichment.tmdb_id;
                show.imdb_id = enrichment.imdb_id.clone();
                show.anilist_id = enrichment.anilist_id;
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
                        episode.air_date = Some(air_date.to_string());
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
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::ShowRepository;
    use super::in_memory::InMemoryShowRepository;
    use crate::models::show::Show;
    use crate::repositories::FileRepository;
    use crate::repositories::contract::fixture::ShowRepositoryFixture;
    use crate::repositories::file::in_memory::InMemoryFileRepository;

    /// The hermetic instantiation of the shared contract: a show double linked
    /// to the file double the contract writes files through.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryShowRepository,
        files: Arc<InMemoryFileRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let files = Arc::new(InMemoryFileRepository::default());
            Self {
                repo: InMemoryShowRepository::with_files(files.clone()),
                files,
            }
        }
    }

    #[async_trait::async_trait]
    impl ShowRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn ShowRepository {
            &self.repo
        }

        fn files(&self) -> &dyn FileRepository {
            self.files.as_ref()
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_unkeyed_show(
            &self,
            title: &str,
            created_at: chrono::DateTime<chrono::Utc>,
        ) -> Uuid {
            let now = created_at;
            let show = Show {
                id: Uuid::new_v4(),
                title: title.to_string(),
                identity_key: None,
                pinned_ref: None,
                title_localized: None,
                description: None,
                year: None,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
                created_at: now,
                updated_at: now,
            };
            let id = show.id;
            self.repo.shows.lock().unwrap().insert(id, show);
            id
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::show_repository_contract!(setup);
}
