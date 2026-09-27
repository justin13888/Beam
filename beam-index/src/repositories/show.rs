use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{CreateEpisode, CreateShow, Episode, Season, Show, ShowSearchQuery};
use beam_domain::providers::enrichment::{SeasonEnrichment, ShowEnrichment};
use beam_domain::repositories::ShowRepository;

/// The `search` condition that keeps only live shows: a present file behind
/// one of the show's episodes.
const LIVE_SHOW: &str = "EXISTS (SELECT 1 FROM seasons se \
     JOIN episodes e ON e.season_id = se.id \
     JOIN files f ON f.episode_id = e.id \
     WHERE se.show_id = shows.id AND f.missing_since IS NULL)";

/// SQL-based implementation of the ShowRepository trait.
#[derive(Debug, Clone)]
pub struct SqlShowRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlShowRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ShowRepository for SqlShowRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::EntityTrait;

        let model = show::Entity::find_by_id(id).one(self.db.as_ref()).await?;
        Ok(model.map(Show::from))
    }

    async fn find_all(&self) -> Result<Vec<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::EntityTrait;

        let models = show::Entity::find().all(self.db.as_ref()).await?;
        Ok(models.into_iter().map(Show::from).collect())
    }

    async fn search(&self, query: &ShowSearchQuery) -> Result<Vec<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{DbBackend, FromQueryResult, Statement, Value};

        // Only live shows: some episode with a present file (issue #183).
        // Binds nothing, so the placeholder numbering below is unaffected.
        let mut conditions: Vec<String> = vec![LIVE_SHOW.to_string()];
        let mut values: Vec<Value> = Vec::new();

        // Pushed first (when present) so its placeholder index is always $1,
        // letting ORDER BY reuse it without recomputing the index.
        if let Some(q) = &query.query {
            values.push(q.clone().into());
            conditions
                .push("(similarity(title, $1) > 0.2 OR title ILIKE '%' || $1 || '%')".to_string());
        }
        if let Some(y) = query.year {
            values.push((y as i32).into());
            conditions.push(format!("year = ${}", values.len()));
        }
        if let Some(yf) = query.year_from {
            values.push((yf as i32).into());
            conditions.push(format!("year >= ${}", values.len()));
        }
        if let Some(yt) = query.year_to {
            values.push((yt as i32).into());
            conditions.push(format!("year <= ${}", values.len()));
        }

        let where_clause = format!("WHERE {}", conditions.join(" AND "));
        let order_by = if query.query.is_some() {
            "ORDER BY similarity(title, $1) DESC, title ASC"
        } else {
            "ORDER BY title ASC"
        };

        let sql = format!("SELECT * FROM shows {where_clause} {order_by}");
        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
        let models = show::Model::find_by_statement(stmt)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Show::from).collect())
    }

    async fn find_or_create_by_identity(&self, create: CreateShow) -> Result<Show, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

        let CreateShow {
            identity_key,
            identity_key_version,
            title,
            year,
        } = create;

        // `ON CONFLICT (identity_key) DO NOTHING` then a read by key, as for
        // movies: two episodes of a new show indexed at once no longer both
        // read "absent" and create two shows.
        let now = Utc::now();
        let active = show::ActiveModel {
            id: Set(Uuid::new_v4()),
            identity_key: Set(Some(identity_key.clone())),
            identity_key_version: Set(identity_key_version as i16),
            title: Set(title),
            year: Set(year.map(|y| y as i32)),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        };
        show::Entity::insert(active)
            .on_conflict_do_nothing_on([show::Column::IdentityKey])
            .exec_without_returning(self.db.as_ref())
            .await?;

        let stored = show::Entity::find()
            .filter(show::Column::IdentityKey.eq(identity_key.as_str()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "show keyed {identity_key:?} is not readable after find-or-create"
                ))
            })?;
        Ok(Show::from(stored))
    }

    async fn find_unkeyed(&self) -> Result<Vec<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        // Oldest first: of two legacy duplicates, the original takes the key.
        let models = show::Entity::find()
            .filter(show::Column::IdentityKey.is_null())
            .order_by_asc(show::Column::CreatedAt)
            .order_by_asc(show::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Show::from).collect())
    }

    async fn assign_identity_key(
        &self,
        show_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr> {
        use beam_entity::show;
        use sea_orm::sea_query::{Expr, Query};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // As for movies: `NOT EXISTS` for the ordinary clash, the unique
        // index for a concurrent one.
        let result = show::Entity::update_many()
            .col_expr(
                show::Column::IdentityKey,
                Expr::value(Some(identity_key.to_string())),
            )
            .col_expr(
                show::Column::IdentityKeyVersion,
                Expr::value(version as i16),
            )
            .filter(show::Column::Id.eq(show_id))
            .filter(show::Column::IdentityKey.is_null())
            .filter(Expr::not_exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from(show::Entity)
                    .and_where(show::Column::IdentityKey.eq(identity_key))
                    .to_owned(),
            ))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(result) => Ok(result.rows_affected == 1),
            Err(err) if super::movie::is_unique_violation(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = show::Entity::find()
            .filter(show::Column::IdentityKey.eq(identity_key))
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Show::from))
    }

    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Show>, DbErr> {
        use beam_entity::show;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let models = show::Entity::find()
            .filter(show::Column::IdentityKey.is_not_null())
            .filter(show::Column::IdentityKeyVersion.lt(version as i16))
            .order_by_asc(show::Column::CreatedAt)
            .order_by_asc(show::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Show::from).collect())
    }

    async fn rekey(
        &self,
        show_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr> {
        use beam_entity::show;
        use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let mut update = show::Entity::update_many()
            .col_expr(show::Column::IdentityKey, Expr::value(identity_key.clone()))
            .col_expr(
                show::Column::IdentityKeyVersion,
                Expr::value(version as i16),
            )
            .filter(show::Column::Id.eq(show_id));
        // As in `assign_identity_key`: `NOT EXISTS` answers the ordinary
        // clash, the unique index a concurrent one. The show itself may
        // already hold the key.
        if let Some(key) = identity_key.as_deref() {
            let other = Alias::new("other");
            update = update.filter(Expr::not_exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from_as(show::Entity, other.clone())
                    .and_where(Expr::col((other.clone(), show::Column::IdentityKey)).eq(key))
                    .and_where(Expr::col((other, show::Column::Id)).ne(show_id))
                    .to_owned(),
            ));
        }
        match update.exec(self.db.as_ref()).await {
            Ok(result) => Ok(result.rows_affected == 1),
            Err(err) if super::movie::is_unique_violation(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let cutoff: sea_orm::prelude::DateTimeWithTimeZone = created_before.into();
        // Episodes no file row references, then seasons left empty, then
        // shows left with no season. `seasons` carries no `created_at`, so
        // this step alone has no cutoff: a season is only ever empty for the
        // moment between the indexer creating it and creating its first
        // episode, and a season the watcher creates in that moment is deleted
        // under it. The watcher's episode insert then fails for that one file,
        // and the next scan recreates the season and indexes it.
        self.db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM episodes e \
                  WHERE e.created_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM files f WHERE f.episode_id = e.id)",
                [cutoff.into()],
            ))
            .await?;
        self.db
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                "DELETE FROM seasons se \
                  WHERE NOT EXISTS (SELECT 1 FROM episodes e WHERE e.season_id = se.id)",
            ))
            .await?;
        // `ON DELETE CASCADE` takes the library association, enrichment state
        // and genre links with the show.
        let shows = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM shows s \
                  WHERE s.created_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM seasons se WHERE se.show_id = s.id)",
                [cutoff.into()],
            ))
            .await?;
        Ok(shows.rows_affected())
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
            .one(self.db.as_ref())
            .await?
            .is_some();

        if !exists {
            let new_assoc = library_show::ActiveModel {
                library_id: Set(library_id),
                show_id: Set(show_id),
            };
            new_assoc.insert(self.db.as_ref()).await?;
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
            .one(self.db.as_ref())
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

        let result = new_season.insert(self.db.as_ref()).await?;
        Ok(Season::from(result))
    }

    async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr> {
        use beam_entity::season;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

        let models = season::Entity::find()
            .filter(season::Column::ShowId.eq(show_id))
            .order_by(season::Column::SeasonNumber, Order::Asc)
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(Season::from).collect())
    }

    async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr> {
        use beam_entity::episode;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

        let models = episode::Entity::find()
            .filter(episode::Column::SeasonId.eq(season_id))
            .order_by(episode::Column::EpisodeNumber, Order::Asc)
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(Episode::from).collect())
    }

    async fn find_or_create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr> {
        use beam_entity::episode;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

        let CreateEpisode {
            season_id,
            episode_number,
            title,
            runtime,
            air_date,
        } = create;

        // One `INSERT ... ON CONFLICT DO NOTHING`, not SELECT-then-INSERT:
        // `(season_id, episode_number)` carries `idx_episodes_unique`, so two
        // files for one episode indexed concurrently would both read "absent"
        // and the second insert would fail. On conflict the existing row is
        // left exactly as it is -- a later file's parse never rewrites the
        // title or runtime -- and read back below.
        let active = episode::ActiveModel {
            id: Set(Uuid::new_v4()),
            season_id: Set(season_id),
            episode_number: Set(episode_number as i32),
            title: Set(title),
            runtime_mins: Set(runtime.map(|d| (d.as_secs() / 60) as i32)),
            air_date: Set(air_date),
            created_at: Set(Utc::now().into()),
            ..Default::default()
        };

        // Inserted or conflicted, the row is read back by its pair: one code
        // path whichever call won. `exec_with_returning` is deliberately not
        // used -- in sea-orm 2.0 a `DO NOTHING` that returns no row surfaces
        // from it as `RecordNotFound`, not as `TryInsertResult::Conflicted`.
        episode::Entity::insert(active)
            .on_conflict_do_nothing_on([episode::Column::SeasonId, episode::Column::EpisodeNumber])
            .exec_without_returning(self.db.as_ref())
            .await?;

        // `ON CONFLICT` returns only once the conflicting row is committed, so
        // under READ COMMITTED this fresh statement sees it.
        let stored = episode::Entity::find()
            .filter(episode::Column::SeasonId.eq(season_id))
            .filter(episode::Column::EpisodeNumber.eq(episode_number as i32))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "episode {episode_number} of season {season_id} is not readable after \
                     find-or-create"
                ))
            })?;
        Ok(Episode::from(stored))
    }

    async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr> {
        use beam_entity::episode;
        use sea_orm::EntityTrait;

        let model = episode::Entity::find_by_id(episode_id)
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Episode::from))
    }

    async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr> {
        use beam_entity::season;
        use sea_orm::EntityTrait;

        let model = season::Entity::find_by_id(season_id)
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Season::from))
    }

    async fn apply_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &ShowEnrichment,
    ) -> Result<(), DbErr> {
        use beam_entity::show;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = show::Entity::find_by_id(show_id)
            .one(self.db.as_ref())
            .await?
        else {
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
        active.update(self.db.as_ref()).await?;
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
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(0);
        };
        let season_id = season_model.id;

        let mut active_season: season::ActiveModel = season_model.into();
        active_season.poster_url = Set(enrichment.poster_url.clone());
        active_season.first_aired = Set(enrichment.air_date);
        active_season.update(self.db.as_ref()).await?;

        let mut updated = 0u32;
        for ep_enrichment in &enrichment.episodes {
            let Some(ep_model) = episode::Entity::find()
                .filter(episode::Column::SeasonId.eq(season_id))
                .filter(episode::Column::EpisodeNumber.eq(ep_enrichment.episode_number as i32))
                .one(self.db.as_ref())
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
            active_ep.update(self.db.as_ref()).await?;
            updated += 1;
        }

        Ok(updated)
    }
}
