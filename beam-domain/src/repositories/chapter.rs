use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::chapter::Chapter;

/// Each video file's chapters (issue #265), backing the `file_chapters`
/// table.
///
/// A file's chapters are written whole, each time it is probed: they are
/// what the container says, so nothing edits one chapter on its own. They go
/// with their file when its row is purged.
#[async_trait]
pub trait ChapterRepository: Send + Sync + std::fmt::Debug {
    /// Replace the chapters of the file `file_id` with `chapters`, whole and
    /// atomically: a reader sees the old set or the new one, never a mix. An
    /// empty list leaves the file with none.
    async fn replace_for_file(&self, file_id: Uuid, chapters: Vec<Chapter>) -> Result<(), DbErr>;

    /// The chapters of the file `file_id`, by index; none for a file with
    /// none, and for an id no file has.
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<Chapter>, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// The in-memory double. It knows nothing of the files a chapter belongs
    /// to, so purging a file does not take its chapters with it here; the
    /// `pg-integration` tier asserts that cascade.
    #[derive(Debug, Default)]
    pub struct InMemoryChapterRepository {
        chapters: RwLock<HashMap<Uuid, Vec<Chapter>>>,
    }

    #[async_trait]
    impl ChapterRepository for InMemoryChapterRepository {
        async fn replace_for_file(
            &self,
            file_id: Uuid,
            mut chapters: Vec<Chapter>,
        ) -> Result<(), DbErr> {
            chapters.sort_by_key(|chapter| chapter.index);
            let mut stored = self.chapters.write();
            if chapters.is_empty() {
                stored.remove(&file_id);
            } else {
                stored.insert(file_id, chapters);
            }
            Ok(())
        }

        async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<Chapter>, DbErr> {
            Ok(self
                .chapters
                .read()
                .get(&file_id)
                .cloned()
                .unwrap_or_default())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::ChapterRepository;
    use super::in_memory::InMemoryChapterRepository;
    use crate::repositories::contract::fixture::ChapterFixture;

    /// The hermetic instantiation of the chapter contract: no foreign key to
    /// satisfy, so every file id is fresh.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryChapterRepository,
    }

    #[async_trait::async_trait]
    impl ChapterFixture for InMemoryFixture {
        fn repo(&self) -> &dyn ChapterRepository {
            &self.repo
        }

        async fn new_file(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::chapter_repository_contract!(setup);
}
