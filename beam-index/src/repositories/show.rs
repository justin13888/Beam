use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{CreateEpisode, CreateShow, Episode, Season, Show};
use beam_domain::providers::enrichment::{SeasonEnrichment, ShowEnrichment};
use beam_domain::repositories::ShowRepository;

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

    async fn create(&self, create: CreateShow) -> Result<Show, DbErr> {
        use beam_entity::show;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_show = show::ActiveModel {
            id: Set(Uuid::new_v4()),
            title: Set(create.title),
            year: Set(create.year.map(|y| y as i32)),
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

    async fn apply_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &ShowEnrichment,
    ) -> Result<(), DbErr> {
        use beam_entity::show;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = show::Entity::find_by_id(show_id).one(&self.db).await? else {
            return Ok(());
        };

        let mut active: show::ActiveModel = model.into();
        active.title = Set(enrichment.title.clone());
        active.title_localized = Set(enrichment.original_title.clone());
        active.description = Set(enrichment.description.clone());
        active.year = Set(enrichment.year.map(|y| y as i32));
        active.poster_url = Set(enrichment.poster_url.clone());
        active.backdrop_url = Set(enrichment.backdrop_url.clone());
        active.tmdb_id = Set(enrichment.tmdb_id.map(|id| id as i32));
        active.imdb_id = Set(enrichment.imdb_id.clone());
        active.anilist_id = Set(enrichment.anilist_id.map(|id| id as i32));
        active.updated_at = Set(chrono::Utc::now().into());
        active.update(&self.db).await?;
        Ok(())
    }

    async fn apply_season_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &SeasonEnrichment,
    ) -> Result<u32, DbErr> {
        use beam_entity::{episode, season};
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let Some(season_model) = season::Entity::find()
            .filter(season::Column::ShowId.eq(show_id))
            .filter(season::Column::SeasonNumber.eq(enrichment.season_number as i32))
            .one(&self.db)
            .await?
        else {
            return Ok(0);
        };
        let season_id = season_model.id;

        let mut active_season: season::ActiveModel = season_model.into();
        active_season.poster_url = Set(enrichment.poster_url.clone());
        active_season.first_aired = Set(enrichment.air_date);
        active_season.update(&self.db).await?;

        let mut updated = 0u32;
        for ep_enrichment in &enrichment.episodes {
            let Some(ep_model) = episode::Entity::find()
                .filter(episode::Column::SeasonId.eq(season_id))
                .filter(episode::Column::EpisodeNumber.eq(ep_enrichment.episode_number as i32))
                .one(&self.db)
                .await?
            else {
                continue;
            };

            let mut active_ep: episode::ActiveModel = ep_model.into();
            if let Some(title) = &ep_enrichment.title {
                active_ep.title = Set(title.clone());
            }
            if ep_enrichment.description.is_some() {
                active_ep.description = Set(ep_enrichment.description.clone());
            }
            if ep_enrichment.air_date.is_some() {
                active_ep.air_date = Set(ep_enrichment.air_date);
            }
            if ep_enrichment.runtime_mins.is_some() {
                active_ep.runtime_mins = Set(ep_enrichment.runtime_mins.map(|m| m as i32));
            }
            if ep_enrichment.thumbnail_url.is_some() {
                active_ep.thumbnail_url = Set(ep_enrichment.thumbnail_url.clone());
            }
            active_ep.update(&self.db).await?;
            updated += 1;
        }

        Ok(updated)
    }
}
