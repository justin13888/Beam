use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateEpisode, Episode, Season, Show};

/// Repository for managing TV show persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr>;
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
    async fn find_by_title(&self, title: &str) -> Result<Option<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = show::Entity::find()
            .filter(show::Column::Title.eq(title))
            .one(&self.db)
            .await?;

        Ok(model.map(Show::from))
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
