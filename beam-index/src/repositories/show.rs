use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateEpisode, Episode, Season, Show};

/// Repository for managing TV show persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Show>, DbErr>;
    async fn create(&self, title: String) -> Result<Show, DbErr>;
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

/// SQL-based implementation of the ShowRepository trait.
#[derive(Debug, Clone)]
pub struct SqlShowRepository {
    db: DatabaseConnection,
}

impl SqlShowRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ShowRepository for SqlShowRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::EntityTrait;

        let model = show::Entity::find_by_id(id).one(&self.db).await?;
        Ok(model.map(Show::from))
    }

    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = show::Entity::find()
            .filter(show::Column::Title.eq(title))
            .one(&self.db)
            .await?;

        Ok(model.map(Show::from))
    }

    async fn find_all(&self) -> Result<Vec<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::EntityTrait;

        let models = show::Entity::find().all(&self.db).await?;
        Ok(models.into_iter().map(Show::from).collect())
    }

    async fn create(&self, title: String) -> Result<Show, DbErr> {
        use beam_entity::show;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_show = show::ActiveModel {
            id: Set(Uuid::new_v4()),
            title: Set(title),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        };

        let result = new_show.insert(&self.db).await?;
        Ok(Show::from(result))
    }

    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        show_id: Uuid,
    ) -> Result<(), DbErr> {
        use beam_entity::library_show;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        // Check if association already exists
        let exists = library_show::Entity::find()
            .filter(library_show::Column::LibraryId.eq(library_id))
            .filter(library_show::Column::ShowId.eq(show_id))
            .one(&self.db)
            .await?
            .is_some();

        if !exists {
            let new_assoc = library_show::ActiveModel {
                library_id: Set(library_id),
                show_id: Set(show_id),
            };
            new_assoc.insert(&self.db).await?;
        }

        Ok(())
    }

    async fn find_or_create_season(
        &self,
        show_id: Uuid,
        season_number: u32,
    ) -> Result<Season, DbErr> {
        use beam_entity::season;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        // Try to find existing season
        let existing = season::Entity::find()
            .filter(season::Column::ShowId.eq(show_id))
            .filter(season::Column::SeasonNumber.eq(season_number as i32))
            .one(&self.db)
            .await?;

        if let Some(model) = existing {
            return Ok(Season::from(model));
        }

        // Create new season
        let new_season = season::ActiveModel {
            id: Set(Uuid::new_v4()),
            show_id: Set(show_id),
            season_number: Set(season_number as i32),
            ..Default::default()
        };

        let result = new_season.insert(&self.db).await?;
        Ok(Season::from(result))
    }

    async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr> {
        use beam_entity::season;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

        let models = season::Entity::find()
            .filter(season::Column::ShowId.eq(show_id))
            .order_by(season::Column::SeasonNumber, Order::Asc)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(Season::from).collect())
    }

    async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr> {
        use beam_entity::episode;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

        let models = episode::Entity::find()
            .filter(episode::Column::SeasonId.eq(season_id))
            .order_by(episode::Column::EpisodeNumber, Order::Asc)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(Episode::from).collect())
    }

    async fn create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr> {
        use beam_entity::episode;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_episode = episode::ActiveModel {
            id: Set(Uuid::new_v4()),
            season_id: Set(create.season_id),
            episode_number: Set(create.episode_number as i32),
            title: Set(create.title),
            runtime_mins: Set(create.runtime.map(|d| (d.as_secs() / 60) as i32)),
            created_at: Set(now.into()),
            ..Default::default()
        };

        let result = new_episode.insert(&self.db).await?;
        Ok(Episode::from(result))
    }
}

/// In-memory implementation for use in tests and test-utils consumers.
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

        async fn create(&self, title: String) -> Result<Show, DbErr> {
            let show = Show {
                id: Uuid::new_v4(),
                title,
                title_localized: None,
                description: None,
                year: None,
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
