use crate::models::domain::{CreateLibrary, Library};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

/// Repository for managing library persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait LibraryRepository: Send + Sync + std::fmt::Debug {
    async fn find_all(&self) -> Result<Vec<Library>, DbErr>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>, DbErr>;
    async fn create(&self, create: CreateLibrary) -> Result<Library, DbErr>;
    async fn count_files(&self, library_id: Uuid) -> Result<u64, DbErr>;
    async fn update_scan_progress(
        &self,
        library_id: Uuid,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        file_count: Option<i32>,
    ) -> Result<(), DbErr>;
    async fn delete(&self, id: Uuid) -> Result<(), DbErr>;
}

/// SQL-based implementation of the LibraryRepository trait.
#[derive(Debug, Clone)]
pub struct SqlLibraryRepository {
    db: DatabaseConnection,
}

impl SqlLibraryRepository {
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
