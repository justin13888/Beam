use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};
use beam_domain::models::{PinSource, ProviderPin};
use beam_domain::providers::enrichment::MovieEnrichment;
use beam_domain::repositories::MovieRepository;

/// Whether `err` is a unique-index violation.
pub(crate) fn is_unique_violation(err: &DbErr) -> bool {
    matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

/// SQL-based implementation of the MovieRepository trait.
#[derive(Debug, Clone)]
pub struct SqlMovieRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlMovieRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MovieRepository for SqlMovieRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::EntityTrait;

        let model = movie::Entity::find_by_id(id).one(self.db.as_ref()).await?;
        Ok(model.map(Movie::from))
    }

    async fn find_all(&self) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::EntityTrait;

        let models = movie::Entity::find().all(self.db.as_ref()).await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let models = movie::Entity::find()
            .filter(movie::Column::Id.is_in(ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn find_or_create_by_identity(&self, create: CreateMovie) -> Result<Movie, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

        let CreateMovie {
            identity_key,
            identity_key_version,
            title,
            year,
            runtime,
        } = create;

        // One `INSERT ... ON CONFLICT (identity_key) DO NOTHING`, then a read
        // by key -- the same shape as `find_or_create_episode`. The former
        // `find_by_title`-then-`create` let two files of one new movie, indexed
        // at once by a scan and the watcher, both read "absent" and create two
        // movies; the unique key makes that impossible. On conflict the stored
        // row is left exactly as it is.
        let now = Utc::now();
        let active = movie::ActiveModel {
            id: Set(Uuid::new_v4()),
            identity_key: Set(Some(identity_key.clone())),
            identity_key_version: Set(identity_key_version as i16),
            title: Set(title),
            year: Set(year.map(|y| y as i32)),
            runtime_mins: Set(runtime.map(|d| (d.as_secs() / 60) as i32)),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        };
        movie::Entity::insert(active)
            .on_conflict_do_nothing_on([movie::Column::IdentityKey])
            .exec_without_returning(self.db.as_ref())
            .await?;

        let stored = movie::Entity::find()
            .filter(movie::Column::IdentityKey.eq(identity_key.as_str()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "movie keyed {identity_key:?} is not readable after find-or-create"
                ))
            })?;
        Ok(Movie::from(stored))
    }

    async fn find_unkeyed(&self) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        // Oldest first: of two legacy duplicates, the original takes the key.
        let models = movie::Entity::find()
            .filter(movie::Column::IdentityKey.is_null())
            .order_by_asc(movie::Column::CreatedAt)
            .order_by_asc(movie::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn assign_identity_key(
        &self,
        movie_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr> {
        use beam_entity::movie;
        use sea_orm::sea_query::{Expr, Query};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // The `NOT EXISTS` answers the ordinary clash without an error; the
        // unique index still settles a race with a concurrent insert of the
        // same key, which surfaces as a violation and is the same answer.
        let result = movie::Entity::update_many()
            .col_expr(
                movie::Column::IdentityKey,
                Expr::value(Some(identity_key.to_string())),
            )
            .col_expr(
                movie::Column::IdentityKeyVersion,
                Expr::value(version as i16),
            )
            .filter(movie::Column::Id.eq(movie_id))
            .filter(movie::Column::IdentityKey.is_null())
            .filter(Expr::not_exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from(movie::Entity)
                    .and_where(movie::Column::IdentityKey.eq(identity_key))
                    .to_owned(),
            ))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(result) => Ok(result.rows_affected == 1),
            Err(err) if is_unique_violation(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = movie::Entity::find()
            .filter(movie::Column::IdentityKey.eq(identity_key))
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Movie::from))
    }

    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let models = movie::Entity::find()
            .filter(movie::Column::IdentityKey.is_not_null())
            .filter(movie::Column::IdentityKeyVersion.lt(version as i16))
            .order_by_asc(movie::Column::CreatedAt)
            .order_by_asc(movie::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn rekey(
        &self,
        movie_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr> {
        use beam_entity::movie;
        use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let mut update = movie::Entity::update_many()
            .col_expr(
                movie::Column::IdentityKey,
                Expr::value(identity_key.clone()),
            )
            .col_expr(
                movie::Column::IdentityKeyVersion,
                Expr::value(version as i16),
            )
            .filter(movie::Column::Id.eq(movie_id));
        // As in `assign_identity_key`: `NOT EXISTS` answers the ordinary
        // clash, the unique index a concurrent one. The movie itself may
        // already hold the key.
        if let Some(key) = identity_key.as_deref() {
            let other = Alias::new("other");
            update = update.filter(Expr::not_exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from_as(movie::Entity, other.clone())
                    .and_where(Expr::col((other.clone(), movie::Column::IdentityKey)).eq(key))
                    .and_where(Expr::col((other, movie::Column::Id)).ne(movie_id))
                    .to_owned(),
            ));
        }
        match update.exec(self.db.as_ref()).await {
            Ok(result) => Ok(result.rows_affected == 1),
            Err(err) if is_unique_violation(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let pinned = movie::Entity::find()
            .filter(movie::Column::PinnedRef.eq(pin.to_ref_string()))
            .one(self.db.as_ref())
            .await?;
        if let Some(pinned) = pinned {
            return Ok(Some(Movie::from(pinned)));
        }
        let matched = match pin {
            ProviderPin::Tmdb(id) => movie::Column::TmdbId.eq(*id as i32),
            ProviderPin::Imdb(id) => movie::Column::ImdbId.eq(id.as_str()),
            ProviderPin::Tvdb(id) => movie::Column::TvdbId.eq(*id as i32),
            ProviderPin::Anilist(id) => movie::Column::AnilistId.eq(*id as i32),
        };
        let model = movie::Entity::find()
            .filter(matched)
            .order_by_asc(movie::Column::CreatedAt)
            .order_by_asc(movie::Column::Id)
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Movie::from))
    }

    async fn set_pinned_ref(
        &self,
        movie_id: Uuid,
        pin: &ProviderPin,
        source: PinSource,
    ) -> Result<bool, DbErr> {
        use beam_entity::movie;
        use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
        use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};

        let stored = pin.to_ref_string();
        let other = Alias::new("other");
        // As in `rekey`: `NOT EXISTS` answers the ordinary clash, the unique
        // index a concurrent one.
        let mut update = movie::Entity::update_many()
            .col_expr(movie::Column::PinnedRef, Expr::value(Some(stored.clone())))
            .col_expr(
                movie::Column::PinSource,
                Expr::value(Some(source.as_str().to_string())),
            )
            .filter(movie::Column::Id.eq(movie_id));
        if source == PinSource::Nfo {
            // An NFO never replaces an administrator's pin (FR-312).
            update = update.filter(
                Condition::any()
                    .add(movie::Column::PinSource.is_null())
                    .add(movie::Column::PinSource.ne(PinSource::Admin.as_str())),
            );
        }
        let result = update
            .filter(Expr::not_exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from_as(movie::Entity, other.clone())
                    .and_where(Expr::col((other.clone(), movie::Column::PinnedRef)).eq(stored))
                    .and_where(Expr::col((other, movie::Column::Id)).ne(movie_id))
                    .to_owned(),
            ))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(result) => Ok(result.rows_affected == 1),
            Err(err) if is_unique_violation(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let cutoff: sea_orm::prelude::DateTimeWithTimeZone = created_before.into();
        // Entries first: a movie is orphaned once no entry is left, and an
        // entry once no file row -- present or soft-deleted -- references it.
        self.db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM movie_entries me \
                  WHERE me.created_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM files f WHERE f.movie_entry_id = me.id)",
                [cutoff.into()],
            ))
            .await?;
        // `ON DELETE CASCADE` takes the library association, enrichment state
        // and genre links with the movie.
        let movies = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM movies m \
                  WHERE m.created_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM movie_entries me WHERE me.movie_id = m.id)",
                [cutoff.into()],
            ))
            .await?;
        Ok(movies.rows_affected())
    }

    async fn find_or_create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr> {
        use beam_entity::movie_entry;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

        let CreateMovieEntry {
            library_id,
            movie_id,
            edition,
            is_primary,
        } = create;

        // One `INSERT ... ON CONFLICT DO NOTHING` on `idx_movie_entries_unique`,
        // which is `NULLS NOT DISTINCT`: a second copy of a film's default
        // edition conflicts with the first instead of creating a second
        // entry. Then a read by the triple, whichever call won.
        let active = movie_entry::ActiveModel {
            id: Set(Uuid::new_v4()),
            library_id: Set(library_id),
            movie_id: Set(movie_id),
            edition: Set(edition.clone()),
            is_primary: Set(is_primary),
            created_at: Set(Utc::now().into()),
        };
        movie_entry::Entity::insert(active)
            .on_conflict_do_nothing_on([
                movie_entry::Column::LibraryId,
                movie_entry::Column::MovieId,
                movie_entry::Column::Edition,
            ])
            .exec_without_returning(self.db.as_ref())
            .await?;

        let edition_matches = match &edition {
            Some(edition) => movie_entry::Column::Edition.eq(edition.as_str()),
            None => movie_entry::Column::Edition.is_null(),
        };
        let stored = movie_entry::Entity::find()
            .filter(movie_entry::Column::LibraryId.eq(library_id))
            .filter(movie_entry::Column::MovieId.eq(movie_id))
            .filter(edition_matches)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "entry of movie {movie_id} in library {library_id} is not readable after \
                     find-or-create"
                ))
            })?;
        Ok(MovieEntry::from(stored))
    }

    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr> {
        use beam_entity::movie_entry;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = movie_entry::Entity::find()
            .filter(movie_entry::Column::MovieId.eq(movie_id))
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MovieEntry::from).collect())
    }

    async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr> {
        use beam_entity::movie_entry;
        use sea_orm::EntityTrait;

        let model = movie_entry::Entity::find_by_id(entry_id)
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(MovieEntry::from))
    }

    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        movie_id: Uuid,
    ) -> Result<(), DbErr> {
        use beam_entity::library_movie;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        // Check if association already exists
        let exists = library_movie::Entity::find()
            .filter(library_movie::Column::LibraryId.eq(library_id))
            .filter(library_movie::Column::MovieId.eq(movie_id))
            .one(self.db.as_ref())
            .await?
            .is_some();

        if !exists {
            let new_assoc = library_movie::ActiveModel {
                library_id: Set(library_id),
                movie_id: Set(movie_id),
            };
            new_assoc.insert(self.db.as_ref()).await?;
        }

        Ok(())
    }

    async fn apply_enrichment(
        &self,
        movie_id: Uuid,
        enrichment: &MovieEnrichment,
    ) -> Result<(), DbErr> {
        use beam_entity::movie;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = movie::Entity::find_by_id(movie_id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };

        let mut active: movie::ActiveModel = model.into();
        active.title = Set(enrichment.title.clone());
        active.title_localized = Set(enrichment.original_title.clone());
        active.description = Set(enrichment.description.clone());
        active.year = Set(enrichment.year.map(|y| y as i32));
        active.release_date = Set(enrichment.release_date);
        active.runtime_mins = Set(enrichment.runtime_mins.map(|m| m as i32));
        active.poster_url = Set(enrichment.poster_url.clone());
        active.backdrop_url = Set(enrichment.backdrop_url.clone());
        active.tmdb_id = Set(enrichment.tmdb_id.map(|id| id as i32));
        active.imdb_id = Set(enrichment.imdb_id.clone());
        active.anilist_id = Set(enrichment.anilist_id.map(|id| id as i32));
        active.rating_tmdb = Set(enrichment.rating);
        active.updated_at = Set(chrono::Utc::now().into());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }
}
