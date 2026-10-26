use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::trickplay::TrickplayRecord;

/// What Beam made of each file's trickplay (issue #265), backing the
/// `file_trickplay` table: one record per file, for the version of the file
/// it was made from. A record goes with its file when the file's row is
/// purged.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait TrickplayRepository: Send + Sync + std::fmt::Debug {
    /// Record `record` as the file's one record, replacing any it had.
    ///
    /// Atomic: concurrent records of one file leave one.
    async fn upsert(&self, record: TrickplayRecord) -> Result<(), DbErr>;

    /// The record of the file `file_id`; `None` before its first, and for an
    /// id no file has.
    async fn find(&self, file_id: Uuid) -> Result<Option<TrickplayRecord>, DbErr>;

    /// The records of every file of `file_ids` that has one, in no order.
    /// An empty list finds nothing and issues no statement.
    async fn find_by_file_ids(&self, file_ids: &[Uuid]) -> Result<Vec<TrickplayRecord>, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::mtime_as_stored;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// The in-memory double, keyed by file as the table is. It knows nothing
    /// of files, so purging one does not take its record with it here; the
    /// `pg-integration` tier asserts that cascade.
    #[derive(Debug, Default)]
    pub struct InMemoryTrickplayRepository {
        records: RwLock<HashMap<Uuid, TrickplayRecord>>,
    }

    #[async_trait]
    impl TrickplayRepository for InMemoryTrickplayRepository {
        async fn upsert(&self, record: TrickplayRecord) -> Result<(), DbErr> {
            // Held at the store's precision, as a `TIMESTAMPTZ` holds it.
            let record = TrickplayRecord {
                generated_at: mtime_as_stored(record.generated_at),
                ..record
            };
            self.records.write().insert(record.file_id, record);
            Ok(())
        }

        async fn find(&self, file_id: Uuid) -> Result<Option<TrickplayRecord>, DbErr> {
            Ok(self.records.read().get(&file_id).cloned())
        }

        async fn find_by_file_ids(&self, file_ids: &[Uuid]) -> Result<Vec<TrickplayRecord>, DbErr> {
            let records = self.records.read();
            Ok(file_ids
                .iter()
                .filter_map(|id| records.get(id).cloned())
                .collect())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::TrickplayRepository;
    use super::in_memory::InMemoryTrickplayRepository;
    use crate::repositories::contract::fixture::TrickplayFixture;

    /// The hermetic instantiation of the trickplay contract: no foreign key
    /// to satisfy, so every file id is fresh.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryTrickplayRepository,
    }

    #[async_trait::async_trait]
    impl TrickplayFixture for InMemoryFixture {
        fn repo(&self) -> &dyn TrickplayRepository {
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

    crate::trickplay_repository_contract!(setup);
}
