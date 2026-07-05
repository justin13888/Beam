use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::show::{CreateEpisode, CreateShow, Episode, Season, Show};

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Show>, DbErr>;
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
    }
}
