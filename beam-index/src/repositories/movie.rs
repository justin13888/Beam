use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};

/// Repository for managing movie persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MovieRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Movie>, DbErr>;
    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr>;
    async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;
    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr>;
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        movie_id: Uuid,
    ) -> Result<(), DbErr>;
}

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

    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr> {
        use beam_entity::movie;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_movie = movie::ActiveModel {
            id: Set(Uuid::new_v4()),
            title: Set(create.title),
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
}
