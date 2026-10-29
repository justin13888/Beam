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

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    pub struct InMemoryLibraryRepository {
        pub libraries: Mutex<HashMap<Uuid, Library>>,
        /// The file store [`LibraryRepository::count_files`] counts from, as
        /// the SQL implementation counts the `files` table.
        files: Arc<InMemoryFileRepository>,
    }

    impl InMemoryLibraryRepository {
        /// A repository whose libraries hold the files `files` stores.
        #[must_use]
        pub fn with_files(files: Arc<InMemoryFileRepository>) -> Self {
            Self {
                libraries: Mutex::default(),
                files,
            }
        }
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
            // Present files only, as the SQL count is.
            Ok(self
                .files
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|file| file.library_id == library_id && file.missing_since.is_none())
                .count() as u64)
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

#[cfg(test)]
mod contract_over_in_memory {
    use std::sync::Arc;

    use super::LibraryRepository;
    use super::in_memory::InMemoryLibraryRepository;
    use crate::repositories::FileRepository;
    use crate::repositories::file::in_memory::InMemoryFileRepository;

    struct InMemoryFixture {
        repo: InMemoryLibraryRepository,
        files: Arc<InMemoryFileRepository>,
    }

    impl LibraryRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn LibraryRepository {
            &self.repo
        }

        fn files(&self) -> &dyn FileRepository {
            self.files.as_ref()
        }
    }

    async fn setup() -> InMemoryFixture {
        let files = Arc::new(InMemoryFileRepository::default());
        InMemoryFixture {
            repo: InMemoryLibraryRepository::with_files(files.clone()),
            files,
        }
    }

    crate::library_repository_contract!(setup);
}
