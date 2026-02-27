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

/// In-memory implementation for use in tests and test-utils consumers.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryLibraryRepository {
        pub libraries: Mutex<HashMap<Uuid, Library>>,
        pub file_counts: Mutex<HashMap<Uuid, u64>>,
    }

    #[async_trait]
    impl LibraryRepository for InMemoryLibraryRepository {
        async fn find_all(&self) -> Result<Vec<Library>, DbErr> {
            Ok(self.libraries.lock().unwrap().values().cloned().collect())
        }

        async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>, DbErr> {
            Ok(self.libraries.lock().unwrap().get(&id).cloned())
        }

        async fn create(&self, create: CreateLibrary) -> Result<Library, DbErr> {
            let library = Library {
                id: Uuid::new_v4(),
                name: create.name,
                root_path: create.root_path,
                description: create.description,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_scan_started_at: None,
                last_scan_finished_at: None,
                last_scan_file_count: None,
            };
            self.libraries
                .lock()
                .unwrap()
                .insert(library.id, library.clone());
            Ok(library)
        }

        async fn count_files(&self, library_id: Uuid) -> Result<u64, DbErr> {
            Ok(*self
                .file_counts
                .lock()
                .unwrap()
                .get(&library_id)
                .unwrap_or(&0))
        }

        async fn update_scan_progress(
            &self,
            library_id: Uuid,
            started_at: Option<DateTime<Utc>>,
            finished_at: Option<DateTime<Utc>>,
            file_count: Option<i32>,
        ) -> Result<(), DbErr> {
            let mut libraries = self.libraries.lock().unwrap();
            if let Some(lib) = libraries.get_mut(&library_id) {
                if started_at.is_some() {
                    lib.last_scan_started_at = started_at;
                }
                if finished_at.is_some() {
                    lib.last_scan_finished_at = finished_at;
                }
                if file_count.is_some() {
                    lib.last_scan_file_count = file_count;
                }
            }
            Ok(())
        }

        async fn delete(&self, id: Uuid) -> Result<(), DbErr> {
            self.libraries.lock().unwrap().remove(&id);
            Ok(())
        }
    }
}
