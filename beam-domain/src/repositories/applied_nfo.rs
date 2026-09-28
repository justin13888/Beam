use std::path::Path;

use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};

/// Persistence for what each NFO held when Beam last applied it (issue #184),
/// backing the `applied_nfos` table. The indexer re-applies an NFO exactly
/// when its content differs from the record here.
///
/// A row is keyed by its path. Nothing references one, so the record of an
/// NFO that is gone is deleted outright; a row goes with its library.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait AppliedNfoRepository: Send + Sync + std::fmt::Debug {
    /// Record `record` under its path: a new row, or the row already at that
    /// path updated in place -- keeping its id and `created_at`.
    ///
    /// Atomic: concurrent records of one path leave one row.
    async fn record_by_path(&self, record: RecordAppliedNfo) -> Result<AppliedNfo, DbErr>;
    async fn find_by_path(&self, path: &Path) -> Result<Option<AppliedNfo>, DbErr>;
    /// Every record in the library `library_id`, by path.
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<AppliedNfo>, DbErr>;
    /// Delete the rows `ids`, returning how many went. An empty list deletes
    /// nothing and issues no statement.
    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr>;
    /// Delete every record in the library `library_id` whose path lies
    /// strictly beneath the directory `dir`, returning how many went.
    /// Beneath by whole path components: `/a/S1` holds `/a/S1/x.nfo` but not
    /// `/a/S10/x.nfo`, and a `_` or `%` in `dir` is itself. The watcher
    /// forgets what a removed directory held with it, as it forgets the
    /// record of a removed NFO (FR-219).
    async fn delete_beneath(&self, library_id: Uuid, dir: &Path) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// The in-memory double, keyed by path as the table is. It knows nothing
    /// of libraries, so deleting one does not take its records with it here;
    /// the `pg-integration` tier asserts that cascade.
    #[derive(Debug, Default)]
    pub struct InMemoryAppliedNfoRepository {
        pub rows: Mutex<HashMap<PathBuf, AppliedNfo>>,
    }

    #[async_trait]
    impl AppliedNfoRepository for InMemoryAppliedNfoRepository {
        async fn record_by_path(&self, record: RecordAppliedNfo) -> Result<AppliedNfo, DbErr> {
            let RecordAppliedNfo {
                library_id,
                path,
                size_bytes,
                content_hash,
                change_stamp,
            } = record;
            let now = chrono::Utc::now();
            let mut rows = self.rows.lock().unwrap();
            let (id, created_at) = rows
                .get(&path)
                .map_or((Uuid::new_v4(), now), |row| (row.id, row.created_at));
            let row = AppliedNfo {
                id,
                library_id,
                path: path.clone(),
                size_bytes,
                content_hash,
                change_stamp,
                created_at,
                updated_at: now,
            };
            rows.insert(path, row.clone());
            Ok(row)
        }

        async fn find_by_path(&self, path: &Path) -> Result<Option<AppliedNfo>, DbErr> {
            Ok(self.rows.lock().unwrap().get(path).cloned())
        }

        async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<AppliedNfo>, DbErr> {
            let mut rows: Vec<AppliedNfo> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|row| row.library_id == library_id)
                .cloned()
                .collect();
            rows.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(rows)
        }

        async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|_, row| !ids.contains(&row.id));
            Ok((before - rows.len()) as u64)
        }

        async fn delete_beneath(&self, library_id: Uuid, dir: &Path) -> Result<u64, DbErr> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|path, row| {
                !(row.library_id == library_id && path != dir && path.starts_with(dir))
            });
            Ok((before - rows.len()) as u64)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::AppliedNfoRepository;
    use super::in_memory::InMemoryAppliedNfoRepository;
    use crate::repositories::contract::fixture::AppliedNfoFixture;

    /// The hermetic instantiation of the shared contract: no foreign keys to
    /// satisfy, so every library id is fresh.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryAppliedNfoRepository,
    }

    #[async_trait::async_trait]
    impl AppliedNfoFixture for InMemoryFixture {
        fn repo(&self) -> &dyn AppliedNfoRepository {
            &self.repo
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::applied_nfo_repository_contract!(setup);
}
