use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::library::{CreateLibrary, Library};

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
    /// Total number of libraries, for the admin status endpoint (issue #85).
    async fn count(&self) -> Result<u64, DbErr>;
}

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

        async fn count(&self) -> Result<u64, DbErr> {
            Ok(self.libraries.lock().unwrap().len() as u64)
        }
    }
}
