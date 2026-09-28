use std::path::Path;

use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::sidecar::{SidecarSubtitle, UpsertSidecarSubtitle};

/// Persistence for the subtitle files found beside indexed videos (issue
/// #184), backing the `sidecar_subtitles` table.
///
/// A row is keyed by its path: a scan upserts what it finds and deletes what
/// it no longer does. Nothing references a row, so a vanished subtitle is
/// deleted outright rather than soft-deleted like a video file. A row goes
/// with its video file when that file is purged.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait SidecarSubtitleRepository: Send + Sync + std::fmt::Debug {
    /// Record `upsert` under its path: a new row, or the row already at that
    /// path updated in place -- keeping its id and `created_at` -- to belong
    /// to `upsert.file_id` with `upsert`'s format, language, flags, size and
    /// modification time -- the last kept, as `files.mtime` is, in whole
    /// microseconds ([`crate::models::file::mtime_as_stored`]).
    ///
    /// Atomic: concurrent upserts of one path leave one row.
    async fn upsert_by_path(&self, upsert: UpsertSidecarSubtitle)
    -> Result<SidecarSubtitle, DbErr>;
    async fn find_by_path(&self, path: &Path) -> Result<Option<SidecarSubtitle>, DbErr>;
    /// Every subtitle of the video file `file_id`, by path.
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr>;
    /// Every subtitle in the library `library_id`, by path.
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr>;
    /// Delete the rows `ids`, returning how many went. An empty list deletes
    /// nothing and issues no statement.
    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::mtime_as_stored;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// The in-memory double, keyed by path as the table is. It knows nothing
    /// of the files a row belongs to, so purging a file does not take its
    /// subtitles with it here; the `pg-integration` tier asserts that
    /// cascade.
    #[derive(Debug, Default)]
    pub struct InMemorySidecarSubtitleRepository {
        pub rows: Mutex<HashMap<PathBuf, SidecarSubtitle>>,
    }

    fn sorted(mut rows: Vec<SidecarSubtitle>) -> Vec<SidecarSubtitle> {
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows
    }

    #[async_trait]
    impl SidecarSubtitleRepository for InMemorySidecarSubtitleRepository {
        async fn upsert_by_path(
            &self,
            upsert: UpsertSidecarSubtitle,
        ) -> Result<SidecarSubtitle, DbErr> {
            let UpsertSidecarSubtitle {
                file_id,
                library_id,
                path,
                info,
                size_bytes,
                mtime,
            } = upsert;
            let now = chrono::Utc::now();
            let mut rows = self.rows.lock().unwrap();
            let (id, created_at) = rows
                .get(&path)
                .map_or((Uuid::new_v4(), now), |row| (row.id, row.created_at));
            let row = SidecarSubtitle {
                id,
                file_id,
                library_id,
                path: path.clone(),
                info,
                size_bytes,
                mtime: mtime.map(mtime_as_stored),
                created_at,
                updated_at: now,
            };
            rows.insert(path, row.clone());
            Ok(row)
        }

        async fn find_by_path(&self, path: &Path) -> Result<Option<SidecarSubtitle>, DbErr> {
            Ok(self.rows.lock().unwrap().get(path).cloned())
        }

        async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr> {
            Ok(sorted(
                self.rows
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|row| row.file_id == file_id)
                    .cloned()
                    .collect(),
            ))
        }

        async fn find_all_by_library(
            &self,
            library_id: Uuid,
        ) -> Result<Vec<SidecarSubtitle>, DbErr> {
            Ok(sorted(
                self.rows
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|row| row.library_id == library_id)
                    .cloned()
                    .collect(),
            ))
        }

        async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|_, row| !ids.contains(&row.id));
            Ok((before - rows.len()) as u64)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::SidecarSubtitleRepository;
    use super::in_memory::InMemorySidecarSubtitleRepository;
    use crate::repositories::contract::fixture::SidecarSubtitleFixture;

    /// The hermetic instantiation of the shared contract: no foreign keys to
    /// satisfy, so every id is fresh.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemorySidecarSubtitleRepository,
    }

    #[async_trait::async_trait]
    impl SidecarSubtitleFixture for InMemoryFixture {
        fn repo(&self) -> &dyn SidecarSubtitleRepository {
            &self.repo
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_video_file(&self, _library_id: Uuid) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::sidecar_subtitle_repository_contract!(setup);
}
