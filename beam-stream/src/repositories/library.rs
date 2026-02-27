use crate::models::domain::{CreateLibrary, Library};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

/// Repository for managing library persistence operations.
///
/// This trait defines the data access layer for media libraries, providing
/// methods to create, retrieve, and query library metadata. All methods
/// return domain models rather than database entities, maintaining clean
/// separation between the data layer and business logic.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait LibraryRepository: Send + Sync + std::fmt::Debug {
    /// Retrieves all libraries from the database.
    ///
    /// # Returns
    ///
    /// A vector of all libraries in the system, or a database error if the query fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use beam_stream::repositories::LibraryRepository;
    /// # async fn example(repo: &dyn LibraryRepository) -> Result<(), sea_orm::DbErr> {
    /// let libraries = repo.find_all().await?;
    /// println!("Found {} libraries", libraries.len());
    /// # Ok(())
    /// # }
    /// ```
    async fn find_all(&self) -> Result<Vec<Library>, DbErr>;

    /// Finds a library by its unique identifier.
    ///
    /// # Arguments
    ///
    /// * `id` - The UUID of the library to find
    ///
    /// # Returns
    ///
    /// * `Ok(Some(Library))` - If the library exists
    /// * `Ok(None)` - If no library with the given ID exists
    /// * `Err(DbErr)` - If a database error occurs
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>, DbErr>;

    /// Creates a new library in the database.
    ///
    /// # Arguments
    ///
    /// * `create` - The parameters for creating the library
    ///
    /// # Returns
    ///
    /// The newly created library with its generated ID and timestamps,
    /// or a database error if creation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use beam_stream::repositories::LibraryRepository;
    /// # use beam_stream::models::domain::CreateLibrary;
    /// # use std::path::PathBuf;
    /// # async fn example(repo: &dyn LibraryRepository) -> Result<(), sea_orm::DbErr> {
    /// let library = repo.create(CreateLibrary {
    ///     name: "My Movies".to_string(),
    ///     root_path: PathBuf::from("/media/movies"),
    ///     description: Some("Personal movie collection".to_string()),
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn create(&self, create: CreateLibrary) -> Result<Library, DbErr>;

    /// Counts the number of files associated with a library.
    ///
    /// # Arguments
    ///
    /// * `library_id` - The UUID of the library
    ///
    /// # Returns
    ///
    /// The count of files in the library, or a database error if the query fails.
    async fn count_files(&self, library_id: Uuid) -> Result<u64, DbErr>;

    /// Updates the scan progress metadata for a library.
    async fn update_scan_progress(
        &self,
        library_id: Uuid,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        file_count: Option<i32>,
    ) -> Result<(), DbErr>;

    /// Deletes a library by its ID.
    async fn delete(&self, id: Uuid) -> Result<(), DbErr>;
}

/// SQL-based implementation of the LibraryRepository trait.
///
/// Uses SeaORM to interact with a relational database (PostgreSQL).
#[derive(Debug, Clone)]
pub struct SqlLibraryRepository {
    db: DatabaseConnection,
}

impl SqlLibraryRepository {
    /// Creates a new SQL library repository.
    ///
    /// # Arguments
    ///
    /// * `db` - The database connection to use for all operations
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl LibraryRepository for SqlLibraryRepository {
    async fn find_all(&self) -> Result<Vec<Library>, DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        let models = library::Entity::find().all(&self.db).await?;
        Ok(models.into_iter().map(Library::from).collect())
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>, DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        let model = library::Entity::find_by_id(id).one(&self.db).await?;
        Ok(model.map(Library::from))
    }

    async fn create(&self, create: CreateLibrary) -> Result<Library, DbErr> {
        use beam_entity::library;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_library = library::ActiveModel {
            id: Set(Uuid::new_v4()),
            name: Set(create.name),
            root_path: Set(create.root_path.to_string_lossy().to_string()),
            description: Set(create.description),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            last_scan_started_at: Set(None),
            last_scan_finished_at: Set(None),
            last_scan_file_count: Set(None),
        };

        let result = new_library.insert(&self.db).await?;
        Ok(Library::from(result))
    }

    async fn count_files(&self, library_id: Uuid) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .count(&self.db)
            .await
    }

    async fn update_scan_progress(
        &self,
        library_id: Uuid,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        file_count: Option<i32>,
    ) -> Result<(), DbErr> {
        use beam_entity::library;
        use sea_orm::{ActiveModelTrait, Set};

        let mut library: library::ActiveModel = library::ActiveModel {
            id: Set(library_id),
            ..Default::default()
        };

        if let Some(started) = started_at {
            library.last_scan_started_at = Set(Some(started.into()));
        }
        if let Some(finished) = finished_at {
            library.last_scan_finished_at = Set(Some(finished.into()));
        }
        if let Some(count) = file_count {
            library.last_scan_file_count = Set(Some(count));
        }

        library.update(&self.db).await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<(), DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        library::Entity::delete_by_id(id).exec(&self.db).await?;
        Ok(())
    }
}
