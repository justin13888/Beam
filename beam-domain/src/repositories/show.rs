use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::show::{CreateEpisode, CreateShow, Episode, Season, Show, ShowSearchQuery};
use crate::providers::enrichment::{SeasonEnrichment, ShowEnrichment};

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Show>, DbErr>;
    /// Server-side filtered/ranked search, mirroring
    /// `MovieRepository::search`.
    async fn search(&self, query: &ShowSearchQuery) -> Result<Vec<Show>, DbErr>;
    async fn create(&self, create: CreateShow) -> Result<Show, DbErr>;
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
    async fn create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr>;
    /// Reverse lookup from a `MediaFileContent::Episode { episode_id }` back
    /// to the episode -- used together with `find_season_by_id` to resolve a
    /// file id to its show for continue-watching.
    async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr>;
    /// Reverse lookup from `Episode::season_id` to the season (and, via
    /// `Season::show_id`, the show).
    async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr>;
    /// Apply enrichment-provider data to an existing show. Overwrites the
    /// current values, same as `MovieRepository::apply_enrichment`.
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

#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryShowRepository {
        pub shows: Mutex<HashMap<Uuid, Show>>,
        pub seasons: Mutex<HashMap<Uuid, Season>>,
        pub episodes: Mutex<HashMap<Uuid, Episode>>,
    }

    #[async_trait]
    impl ShowRepository for InMemoryShowRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr> {
            Ok(self.shows.lock().unwrap().get(&id).cloned())
        }

        async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr> {
            Ok(self
                .shows
                .lock()
                .unwrap()
                .values()
                .find(|s| s.title == title)
                .cloned())
        }

        async fn find_all(&self) -> Result<Vec<Show>, DbErr> {
            Ok(self.shows.lock().unwrap().values().cloned().collect())
        }

        async fn search(&self, query: &ShowSearchQuery) -> Result<Vec<Show>, DbErr> {
            use crate::models::search::title_match_score;

            let mut scored: Vec<(f64, Show)> = self
                .shows
                .lock()
                .unwrap()
                .values()
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

        async fn create(&self, create: CreateShow) -> Result<Show, DbErr> {
            let show = Show {
                id: Uuid::new_v4(),
                title: create.title,
                title_localized: None,
                description: None,
                year: create.year,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.shows.lock().unwrap().insert(show.id, show.clone());
            Ok(show)
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

        async fn create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr> {
            let ep = Episode {
                id: Uuid::new_v4(),
                season_id: create.season_id,
                episode_number: create.episode_number,
                title: create.title,
                description: None,
                air_date: None,
                runtime: create.runtime,
                thumbnail_url: None,
                created_at: chrono::Utc::now(),
            };
            self.episodes.lock().unwrap().insert(ep.id, ep.clone());
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
