use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};
use uuid::Uuid;

use beam_domain::models::enrichment::{EnrichmentState, EnrichmentTargetId};
use beam_domain::repositories::EnrichmentStateRepository;

/// SQL-based implementation of the EnrichmentStateRepository trait.
#[derive(Debug, Clone)]
pub struct SqlEnrichmentStateRepository {
    db: DatabaseConnection,
}

impl SqlEnrichmentStateRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl EnrichmentStateRepository for SqlEnrichmentStateRepository {
    async fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let query = metadata_enrichment::Entity::find();
        let query = match target {
            EnrichmentTargetId::Movie(id) => {
                query.filter(metadata_enrichment::Column::MovieId.eq(id))
            }
            EnrichmentTargetId::Show(id) => {
                query.filter(metadata_enrichment::Column::ShowId.eq(id))
            }
        };
        if query.one(&self.db).await?.is_some() {
            return Ok(());
        }

        let now = Utc::now();
        let (movie_id, show_id) = match target {
            EnrichmentTargetId::Movie(id) => (Some(id), None),
            EnrichmentTargetId::Show(id) => (None, Some(id)),
        };
        let row = metadata_enrichment::ActiveModel {
            id: Set(Uuid::new_v4()),
            movie_id: Set(movie_id),
            show_id: Set(show_id),
            status: Set(metadata_enrichment::EnrichmentStatus::Pending),
            attempts: Set(0),
            next_attempt_at: Set(None),
            enriched_at: Set(None),
            match_confidence: Set(None),
            matched_ref: Set(None),
            force_refresh: Set(false),
            last_error: Set(None),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };
        row.insert(&self.db).await?;
        Ok(())
    }

    async fn backfill_missing(&self) -> Result<u64, DbErr> {
        // Self-contained INSERT..SELECT: create a pending row for any
        // movie/show that doesn't have one yet. A one-time catch-up for
        // titles indexed before enrichment existed; new titles get their row
        // from `ensure_pending` at classification time instead.
        let stmt = Statement::from_string(
            self.db.get_database_backend(),
            "INSERT INTO metadata_enrichment \
                 (id, movie_id, show_id, status, attempts, force_refresh, created_at, updated_at) \
             SELECT gen_random_uuid(), m.id, NULL, 'pending', 0, false, now(), now() \
               FROM movies m \
              WHERE NOT EXISTS (SELECT 1 FROM metadata_enrichment e WHERE e.movie_id = m.id) \
             UNION ALL \
             SELECT gen_random_uuid(), NULL, s.id, 'pending', 0, false, now(), now() \
               FROM shows s \
              WHERE NOT EXISTS (SELECT 1 FROM metadata_enrichment e WHERE e.show_id = s.id)"
                .to_string(),
        );
        let result = self.db.execute(stmt).await?;
        Ok(result.rows_affected())
    }

    async fn fetch_due(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<EnrichmentState>, DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{
            ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
        };

        let models = metadata_enrichment::Entity::find()
            .filter(
                metadata_enrichment::Column::Status
                    .eq(metadata_enrichment::EnrichmentStatus::Pending),
            )
            .filter(
                Condition::any()
                    .add(metadata_enrichment::Column::NextAttemptAt.is_null())
                    .add(metadata_enrichment::Column::NextAttemptAt.lte(now)),
            )
            .order_by(metadata_enrichment::Column::NextAttemptAt, Order::Asc)
            .limit(limit as u64)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(EnrichmentState::from).collect())
    }

    async fn mark_enriched(
        &self,
        id: Uuid,
        matched_ref: &str,
        confidence: f32,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        if let Some(model) = metadata_enrichment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Enriched);
            active.matched_ref = Set(Some(matched_ref.to_string()));
            active.match_confidence = Set(Some(confidence));
            active.enriched_at = Set(Some(now.into()));
            active.next_attempt_at = Set(None);
            active.force_refresh = Set(false);
            active.last_error = Set(None);
            active.updated_at = Set(now.into());
            active.update(&self.db).await?;
        }
        Ok(())
    }

    async fn mark_unmatched(
        &self,
        id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        if let Some(model) = metadata_enrichment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Unmatched);
            active.last_error = Set(Some(reason.to_string()));
            active.next_attempt_at = Set(None);
            active.enriched_at = Set(None);
            active.updated_at = Set(now.into());
            active.update(&self.db).await?;
        }
        Ok(())
    }

    async fn mark_retrying(
        &self,
        id: Uuid,
        error: &str,
        attempts: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        if let Some(model) = metadata_enrichment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Pending);
            active.attempts = Set(attempts as i32);
            active.next_attempt_at = Set(Some(next_attempt_at.into()));
            active.last_error = Set(Some(error.to_string()));
            active.updated_at = Set(Utc::now().into());
            active.update(&self.db).await?;
        }
        Ok(())
    }

    async fn mark_failed(&self, id: Uuid, error: &str, now: DateTime<Utc>) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        if let Some(model) = metadata_enrichment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Failed);
            active.last_error = Set(Some(error.to_string()));
            active.next_attempt_at = Set(None);
            active.updated_at = Set(now.into());
            active.update(&self.db).await?;
        }
        Ok(())
    }

    async fn request_refresh(
        &self,
        target: EnrichmentTargetId,
        rematch: bool,
    ) -> Result<bool, DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let query = metadata_enrichment::Entity::find();
        let query = match target {
            EnrichmentTargetId::Movie(id) => {
                query.filter(metadata_enrichment::Column::MovieId.eq(id))
            }
            EnrichmentTargetId::Show(id) => {
                query.filter(metadata_enrichment::Column::ShowId.eq(id))
            }
        };
        let Some(model) = query.one(&self.db).await? else {
            return Ok(false);
        };

        let mut active: metadata_enrichment::ActiveModel = model.into();
        active.status = Set(metadata_enrichment::EnrichmentStatus::Pending);
        active.force_refresh = Set(true);
        active.attempts = Set(0);
        active.next_attempt_at = Set(None);
        active.updated_at = Set(Utc::now().into());
        if rematch {
            active.matched_ref = Set(None);
        }
        active.update(&self.db).await?;
        Ok(true)
    }

    async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let models = metadata_enrichment::Entity::find().all(&self.db).await?;
        let count = models.len() as u64;
        for model in models {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Pending);
            active.force_refresh = Set(true);
            active.attempts = Set(0);
            active.next_attempt_at = Set(None);
            active.updated_at = Set(Utc::now().into());
            if rematch {
                active.matched_ref = Set(None);
            }
            active.update(&self.db).await?;
        }
        Ok(count)
    }
}
