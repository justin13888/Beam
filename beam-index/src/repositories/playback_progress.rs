use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::playback_progress::{PlaybackProgress, UpsertPlaybackProgress};
use beam_domain::repositories::PlaybackProgressRepository;
use beam_domain::services::{Clock, RealClock};

/// SQL-based implementation of the PlaybackProgressRepository trait.
#[derive(Debug, Clone)]
pub struct SqlPlaybackProgressRepository {
    db: Arc<DatabaseConnection>,
    /// Source of the `updated_at` stamp. Injected rather than read from
    /// `Utc::now()` so the shared behavioural contract can assert ordering by
    /// advancing a `TestClock` instead of sleeping.
    clock: Arc<dyn Clock>,
}

impl SqlPlaybackProgressRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

#[async_trait]
impl PlaybackProgressRepository for SqlPlaybackProgressRepository {
    async fn upsert(&self, upsert: UpsertPlaybackProgress) -> Result<PlaybackProgress, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::sea_query::OnConflict;
        use sea_orm::{EntityTrait, Set};

        // One statement, not SELECT-then-UPDATE-or-INSERT. The (user_id,
        // file_id) pair carries a unique index, so two concurrent reports for
        // the same pair would race the read-modify-write: both read "absent",
        // both insert, and one fails with a unique violation. `ON CONFLICT`
        // makes the operation atomic and idempotent instead.
        let active = playback_progress::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(upsert.user_id),
            file_id: Set(upsert.file_id),
            position_secs: Set(upsert.position_secs),
            duration_secs: Set(upsert.duration_secs),
            completed: Set(upsert.is_completed()),
            updated_at: Set(self.clock.now().into()),
        };

        let model = playback_progress::Entity::insert(active)
            .on_conflict(
                OnConflict::columns([
                    playback_progress::Column::UserId,
                    playback_progress::Column::FileId,
                ])
                .update_columns([
                    playback_progress::Column::PositionSecs,
                    playback_progress::Column::DurationSecs,
                    playback_progress::Column::Completed,
                    playback_progress::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await?;

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
            .one(self.db.as_ref())
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
            .all(self.db.as_ref())
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
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(PlaybackProgress::from).collect())
    }

    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr> {
        use beam_entity::playback_progress;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        playback_progress::Entity::find()
            .filter(playback_progress::Column::UserId.eq(user_id))
            .count(self.db.as_ref())
            .await
    }
}
