use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};

/// Repository for managing movie persistence operations.
///
/// This trait defines the data access layer for movies and their library associations.
/// Movies can exist in multiple libraries with different editions (e.g., theatrical cut,
/// director's cut), which are represented by `MovieEntry` records.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait MovieRepository: Send + Sync + std::fmt::Debug {
    /// Finds a movie by its title.
    ///
    /// # Arguments
    ///
    /// * `title` - The title to search for (exact match)
    ///
    /// # Returns
    ///
    /// * `Ok(Some(Movie))` - If a movie with the given title exists
    /// * `Ok(None)` - If no movie with the given title exists
    /// * `Err(DbErr)` - If a database error occurs
    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr>;

    /// Creates a new movie in the database.
    ///
    /// # Arguments
    ///
    /// * `create` - The parameters for creating the movie
    ///
    /// # Returns
    ///
    /// The newly created movie with its generated ID and timestamps,
    /// or a database error if creation fails.
    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr>;

    /// Creates a movie entry linking a movie to a library.
    ///
    /// Movie entries allow the same movie to exist in multiple libraries,
    /// potentially with different editions (e.g., theatrical vs. director's cut).
    ///
    /// # Arguments
    ///
    /// * `create` - The parameters for creating the movie entry
    ///
    /// # Returns
    ///
    /// The newly created movie entry, or a database error if creation fails.
    async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;

    /// Ensures a many-to-many association exists between a library and a movie.
    ///
    /// This method is idempotent - it will create the association if it doesn't exist,
    /// or do nothing if it already exists.
    ///
    /// # Arguments
    ///
    /// * `library_id` - The UUID of the library
    /// * `movie_id` - The UUID of the movie
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the association exists or was created successfully
    /// * `Err(DbErr)` - If a database error occurs
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        movie_id: Uuid,
    ) -> Result<(), DbErr>;
}

/// SQL-based implementation of the MovieRepository trait.
///
/// Uses SeaORM to interact with a relational database (PostgreSQL).
#[derive(Debug, Clone)]
pub struct SqlMovieRepository {
    db: DatabaseConnection,
}

impl SqlMovieRepository {
    /// Creates a new SQL movie repository.
    ///
    /// # Arguments
    ///
    /// * `db` - The database connection to use for all operations
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MovieRepository for SqlMovieRepository {
    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr> {
        use beam_entity::movie;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = movie::Entity::find()
            .filter(movie::Column::Title.eq(title))
            .one(&self.db)
            .await?;

        Ok(model.map(Movie::from))
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
