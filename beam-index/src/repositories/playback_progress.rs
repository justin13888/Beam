use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::playback_progress::{PlaybackProgress, UpsertPlaybackProgress};
use beam_domain::repositories::PlaybackProgressRepository;

/// SQL-based implementation of the PlaybackProgressRepository trait.
#[derive(Debug, Clone)]
pub struct SqlPlaybackProgressRepository {
    db: DatabaseConnection,
}

impl SqlPlaybackProgressRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PlaybackProgressRepository for SqlPlaybackProgressRepository {
    async fn upsert(&self, upsert: UpsertPlaybackProgress) -> Result<PlaybackProgress, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let completed = upsert.is_completed();
        let now = chrono::Utc::now();

        let existing = playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(upsert.user_id))
            .filter(playback_progress::Column::FileId.eq(upsert.file_id))
            .one(&self.db)
            .await?;

        let model = if let Some(existing) = existing {
            let mut active: playback_progress::ActiveModel = existing.into();
            active.position_secs = Set(upsert.position_secs);
            active.duration_secs = Set(upsert.duration_secs);
            active.completed = Set(completed);
            active.updated_at = Set(now.into());
            active.update(&self.db).await?
        } else {
            let active = playback_progress::ActiveModel {
                id: Set(Uuid::new_v4()),
                user_id: Set(upsert.user_id),
                file_id: Set(upsert.file_id),
                position_secs: Set(upsert.position_secs),
                duration_secs: Set(upsert.duration_secs),
                completed: Set(completed),
                updated_at: Set(now.into()),
            };
            active.insert(&self.db).await?
        };

        Ok(PlaybackProgress::from(model))
    }

    async fn find_by_user_and_file(
        &self,
        user_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<PlaybackProgress>, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(user_id))
            .filter(playback_progress::Column::FileId.eq(file_id))
            .one(&self.db)
            .await?;

        Ok(model.map(PlaybackProgress::from))
    }

    async fn find_in_progress_by_user(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> Result<Vec<PlaybackProgress>, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};

        let models = playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(user_id))
            .filter(playback_progress::Column::Completed.eq(false))
            .order_by(playback_progress::Column::UpdatedAt, Order::Desc)
            .limit(limit as u64)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(PlaybackProgress::from).collect())
    }

    async fn find_page_by_user(
        &self,
        user_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlaybackProgress>, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};

        let models = playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(user_id))
            .order_by(playback_progress::Column::UpdatedAt, Order::Desc)
            .offset(offset)
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(PlaybackProgress::from).collect())
    }

    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(user_id))
            .count(&self.db)
            .await
    }
}
