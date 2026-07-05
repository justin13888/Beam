use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{CreateMovie, CreateMovieEntry, Movie, MovieEntry, MovieSearchQuery};
use beam_domain::providers::enrichment::MovieEnrichment;
use beam_domain::repositories::MovieRepository;

/// SQL-based implementation of the MovieRepository trait.
#[derive(Debug, Clone)]
pub struct SqlMovieRepository {
    db: DatabaseConnection,
}

impl SqlMovieRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MovieRepository for SqlMovieRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::EntityTrait;

        let model = movie::Entity::find_by_id(id).one(&self.db).await?;
        Ok(model.map(Movie::from))
    }

    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = movie::Entity::find()
            .filter(movie::Column::Title.eq(title))
            .one(&self.db)
            .await?;

        Ok(model.map(Movie::from))
    }

    async fn find_all(&self) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::EntityTrait;

        let models = movie::Entity::find().all(&self.db).await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn search(&self, query: &MovieSearchQuery) -> Result<Vec<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{DbBackend, FromQueryResult, Statement, Value};

        let mut conditions: Vec<String> = Vec::new();
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
        if let Some(min_r) = query.min_rating {
            values.push((min_r as i32).into());
            conditions.push(format!(
                "COALESCE(rating_tmdb * 10, 0) >= ${}",
                values.len()
            ));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let order_by = if query.query.is_some() {
            "ORDER BY similarity(title, $1) DESC, title ASC"
        } else {
            "ORDER BY title ASC"
        };

        let sql = format!("SELECT * FROM movies {where_clause} {order_by}");
        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
        let models = movie::Model::find_by_statement(stmt).all(&self.db).await?;
        Ok(models.into_iter().map(Movie::from).collect())
    }

    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr> {
        use beam_entity::movie;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_movie = movie::ActiveModel {
            id: Set(Uuid::new_v4()),
            title: Set(create.title),
            year: Set(create.year.map(|y| y as i32)),
            runtime_mins: Set(create.runtime.map(|d| (d.as_secs() / 60) as i32)),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        };

        let result = new_movie.insert(&self.db).await?;
        Ok(Movie::from(result))
    }

    async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr> {
        use beam_entity::movie_entry;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_entry = movie_entry::ActiveModel {
            id: Set(Uuid::new_v4()),
            library_id: Set(create.library_id),
            movie_id: Set(create.movie_id),
            edition: Set(create.edition),
            is_primary: Set(create.is_primary),
            created_at: Set(now.into()),
        };

        let result = new_entry.insert(&self.db).await?;
        Ok(MovieEntry::from(result))
    }

    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr> {
        use beam_entity::movie_entry;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = movie_entry::Entity::find()
            .filter(movie_entry::Column::MovieId.eq(movie_id))
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(MovieEntry::from).collect())
    }

    async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr> {
        use beam_entity::movie_entry;
        use sea_orm::EntityTrait;

        let model = movie_entry::Entity::find_by_id(entry_id)
            .one(&self.db)
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
            .one(&self.db)
            .await?
            .is_some();

        if !exists {
            let new_assoc = library_movie::ActiveModel {
                library_id: Set(library_id),
                movie_id: Set(movie_id),
            };
            new_assoc.insert(&self.db).await?;
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

        let Some(model) = movie::Entity::find_by_id(movie_id).one(&self.db).await? else {
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
        active.update(&self.db).await?;
        Ok(())
    }
}
