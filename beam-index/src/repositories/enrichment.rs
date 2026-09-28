use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};
use uuid::Uuid;

use beam_domain::models::catalog::TitleKind;
use beam_domain::models::enrichment::{
    EnrichmentListFilter, EnrichmentListQuery, EnrichmentState, EnrichmentStatusCounts,
    EnrichmentTargetId, FieldLocks,
};
use beam_domain::repositories::EnrichmentStateRepository;
use beam_entity::metadata_enrichment;

/// SQL-based implementation of the EnrichmentStateRepository trait.
#[derive(Debug, Clone)]
pub struct SqlEnrichmentStateRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlEnrichmentStateRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// The condition naming `target`'s row.
fn target_condition(target: EnrichmentTargetId) -> sea_orm::sea_query::SimpleExpr {
    use sea_orm::ColumnTrait;
    match target {
        EnrichmentTargetId::Movie(id) => metadata_enrichment::Column::MovieId.eq(id),
        EnrichmentTargetId::Show(id) => metadata_enrichment::Column::ShowId.eq(id),
    }
}

/// The condition admitting what `filter` admits.
fn filter_condition(filter: &EnrichmentListFilter) -> sea_orm::Condition {
    use sea_orm::{ColumnTrait, Condition};
    let EnrichmentListFilter { status, kind } = *filter;
    let mut condition = Condition::all();
    if let Some(status) = status {
        condition = condition.add(
            metadata_enrichment::Column::Status
                .eq(metadata_enrichment::EnrichmentStatus::from(status)),
        );
    }
    match kind {
        Some(TitleKind::Movie) => {
            condition = condition.add(metadata_enrichment::Column::MovieId.is_not_null());
        }
        Some(TitleKind::Show) => {
            condition = condition.add(metadata_enrichment::Column::ShowId.is_not_null());
        }
        None => {}
    }
    condition
}

/// The `UPDATE` that queues rows for another pass, as `request_refresh`
/// describes, before its `WHERE`.
fn queue_update(rematch: bool) -> sea_orm::UpdateMany<metadata_enrichment::Entity> {
    use sea_orm::ActiveEnum;
    use sea_orm::EntityTrait;
    use sea_orm::sea_query::Expr;
    let mut update = metadata_enrichment::Entity::update_many()
        .col_expr(
            metadata_enrichment::Column::Status,
            metadata_enrichment::EnrichmentStatus::Pending.as_enum(),
        )
        .col_expr(metadata_enrichment::Column::ForceRefresh, Expr::value(true))
        .col_expr(metadata_enrichment::Column::Attempts, Expr::value(0))
        .col_expr(
            metadata_enrichment::Column::NextAttemptAt,
            Expr::value(Option::<chrono::DateTime<chrono::FixedOffset>>::None),
        )
        .col_expr(
            metadata_enrichment::Column::UpdatedAt,
            Expr::value(chrono::DateTime::<chrono::FixedOffset>::from(Utc::now())),
        );
    if rematch {
        update = update.col_expr(
            metadata_enrichment::Column::MatchedRef,
            Expr::value(Option::<String>::None),
        );
    }
    update
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
        if query.one(self.db.as_ref()).await?.is_some() {
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
            locked_fields: Set(Vec::new()),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };
        row.insert(self.db.as_ref()).await?;
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
        let result = self.db.execute_raw(stmt).await?;
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
            .all(self.db.as_ref())
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
            .one(self.db.as_ref())
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
            active.update(self.db.as_ref()).await?;
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
            .one(self.db.as_ref())
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Unmatched);
            active.last_error = Set(Some(reason.to_string()));
            active.next_attempt_at = Set(None);
            active.enriched_at = Set(None);
            active.updated_at = Set(now.into());
            active.update(self.db.as_ref()).await?;
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
            .one(self.db.as_ref())
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Pending);
            active.attempts = Set(attempts as i32);
            active.next_attempt_at = Set(Some(next_attempt_at.into()));
            active.last_error = Set(Some(error.to_string()));
            active.updated_at = Set(Utc::now().into());
            active.update(self.db.as_ref()).await?;
        }
        Ok(())
    }

    async fn mark_failed(&self, id: Uuid, error: &str, now: DateTime<Utc>) -> Result<(), DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        if let Some(model) = metadata_enrichment::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
        {
            let mut active: metadata_enrichment::ActiveModel = model.into();
            active.status = Set(metadata_enrichment::EnrichmentStatus::Failed);
            active.last_error = Set(Some(error.to_string()));
            active.next_attempt_at = Set(None);
            active.updated_at = Set(now.into());
            active.update(self.db.as_ref()).await?;
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
        let Some(model) = query.one(self.db.as_ref()).await? else {
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
        active.update(self.db.as_ref()).await?;
        Ok(true)
    }

    async fn request_refresh_many(
        &self,
        targets: &[EnrichmentTargetId],
        rematch: bool,
    ) -> Result<u64, DbErr> {
        use sea_orm::{ColumnTrait, Condition, QueryFilter};

        if targets.is_empty() {
            return Ok(0);
        }
        let mut movies = Vec::new();
        let mut shows = Vec::new();
        for target in targets {
            match *target {
                EnrichmentTargetId::Movie(id) => movies.push(id),
                EnrichmentTargetId::Show(id) => shows.push(id),
            }
        }
        let mut which = Condition::any();
        if !movies.is_empty() {
            which = which.add(metadata_enrichment::Column::MovieId.is_in(movies));
        }
        if !shows.is_empty() {
            which = which.add(metadata_enrichment::Column::ShowId.is_in(shows));
        }
        let result = queue_update(rematch)
            .filter(which)
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }

    async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr> {
        // One statement: a library of tens of thousands of titles used to be
        // read whole and written back a row at a time.
        let result = queue_update(rematch).exec(self.db.as_ref()).await?;
        Ok(result.rows_affected)
    }

    async fn find_by_target(
        &self,
        target: EnrichmentTargetId,
    ) -> Result<Option<EnrichmentState>, DbErr> {
        use sea_orm::{EntityTrait, QueryFilter};

        Ok(metadata_enrichment::Entity::find()
            .filter(target_condition(target))
            .one(self.db.as_ref())
            .await?
            .map(EnrichmentState::from))
    }

    async fn set_locked_fields(
        &self,
        target: EnrichmentTargetId,
        locks: &FieldLocks,
    ) -> Result<EnrichmentState, DbErr> {
        use sea_orm::sea_query::OnConflict;
        use sea_orm::{EntityTrait, Set};

        let now: chrono::DateTime<chrono::FixedOffset> = Utc::now().into();
        let (movie_id, show_id, conflict) = match target {
            EnrichmentTargetId::Movie(id) => (Some(id), None, metadata_enrichment::Column::MovieId),
            EnrichmentTargetId::Show(id) => (None, Some(id), metadata_enrichment::Column::ShowId),
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
            locked_fields: Set(locks.to_stored()),
            created_at: Set(now),
            updated_at: Set(now),
        };
        // One `INSERT ... ON CONFLICT (movie_id | show_id) DO UPDATE`: a title
        // with a row keeps it -- its status and match untouched -- and one
        // without gets a pending row, however two administrators race.
        let model = metadata_enrichment::Entity::insert(row)
            .on_conflict(
                OnConflict::column(conflict)
                    .update_columns([
                        metadata_enrichment::Column::LockedFields,
                        metadata_enrichment::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await?;
        Ok(EnrichmentState::from(model))
    }

    async fn list(&self, query: &EnrichmentListQuery) -> Result<Vec<EnrichmentState>, DbErr> {
        use sea_orm::{
            ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
        };

        let EnrichmentListQuery {
            filter,
            after,
            limit,
        } = *query;
        let mut select = metadata_enrichment::Entity::find().filter(filter_condition(&filter));
        if let Some(after) = after {
            let at: chrono::DateTime<chrono::FixedOffset> = after.updated_at.into();
            select = select.filter(
                Condition::any()
                    .add(metadata_enrichment::Column::UpdatedAt.lt(at))
                    .add(
                        Condition::all()
                            .add(metadata_enrichment::Column::UpdatedAt.eq(at))
                            .add(metadata_enrichment::Column::Id.lt(after.id)),
                    ),
            );
        }
        let models = select
            .order_by(metadata_enrichment::Column::UpdatedAt, Order::Desc)
            .order_by(metadata_enrichment::Column::Id, Order::Desc)
            .limit(u64::from(limit.get()))
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(EnrichmentState::from).collect())
    }

    async fn count(&self, filter: &EnrichmentListFilter) -> Result<u64, DbErr> {
        use sea_orm::{EntityTrait, PaginatorTrait, QueryFilter};

        metadata_enrichment::Entity::find()
            .filter(filter_condition(filter))
            .count(self.db.as_ref())
            .await
    }

    async fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr> {
        use beam_entity::metadata_enrichment;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        // Four filtered counts rather than a hand-rolled GROUP BY statement:
        // this endpoint is a low-traffic admin status read, and staying on the
        // typed query builder keeps the status enum mapping in one place.
        let count_for = |status: metadata_enrichment::EnrichmentStatus| {
            metadata_enrichment::Entity::find()
                .filter(metadata_enrichment::Column::Status.eq(status))
                .count(self.db.as_ref())
        };

        Ok(EnrichmentStatusCounts {
            pending: count_for(metadata_enrichment::EnrichmentStatus::Pending).await?,
            enriched: count_for(metadata_enrichment::EnrichmentStatus::Enriched).await?,
            unmatched: count_for(metadata_enrichment::EnrichmentStatus::Unmatched).await?,
            failed: count_for(metadata_enrichment::EnrichmentStatus::Failed).await?,
        })
    }
}
