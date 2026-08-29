use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::playback_progress::{PlaybackProgress, UpsertPlaybackProgress};

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait PlaybackProgressRepository: Send + Sync + std::fmt::Debug {
    /// Insert or update the (user, file) progress row, recomputing
    /// `completed` from the reported position/duration.
    async fn upsert(&self, upsert: UpsertPlaybackProgress) -> Result<PlaybackProgress, DbErr>;

    async fn find_by_user_and_file(
        &self,
        user_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<PlaybackProgress>, DbErr>;

    /// In-progress (not `completed`) rows for a user, most-recently-updated
    /// first, for the continue-watching list.
    async fn find_in_progress_by_user(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> Result<Vec<PlaybackProgress>, DbErr>;

    /// One page of a user's watch history, most-recently-updated first.
    /// Unlike [`find_in_progress_by_user`], this includes `completed` rows —
    /// the history view lists everything the user has watched.
    async fn find_page_by_user(
        &self,
        user_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlaybackProgress>, DbErr>;

    /// Total number of history rows for a user (completed and in-progress),
    /// for paginating [`find_page_by_user`].
    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr>;
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build. See
/// [`crate::services::clock::in_memory`] for why the `#[mutants::skip]` is
/// required.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::services::Clock;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory stand-in for the SQL repository.
    ///
    /// Takes the same [`Clock`] the real repository takes, so `updated_at`
    /// ordering is driven by an advanced [`crate::services::TestClock`] rather
    /// than by wall-clock time -- which is what lets the shared contract in
    /// [`super::contract`] assert ordering without sleeping.
    #[derive(Debug)]
    pub struct InMemoryPlaybackProgressRepository {
        rows: Mutex<HashMap<Uuid, PlaybackProgress>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryPlaybackProgressRepository {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                rows: Mutex::new(HashMap::new()),
                clock,
            }
        }
    }

    impl Default for InMemoryPlaybackProgressRepository {
        fn default() -> Self {
            Self::new(Arc::new(crate::services::RealClock))
        }
    }

    #[async_trait]
    impl PlaybackProgressRepository for InMemoryPlaybackProgressRepository {
        async fn upsert(&self, upsert: UpsertPlaybackProgress) -> Result<PlaybackProgress, DbErr> {
            let completed = upsert.is_completed();
            let now = self.clock.now();
            let mut rows = self.rows.lock().unwrap();
            let existing = rows
                .values_mut()
                .find(|r| r.user_id == upsert.user_id && r.file_id == upsert.file_id);

            if let Some(row) = existing {
                row.position_secs = upsert.position_secs;
                row.duration_secs = upsert.duration_secs;
                row.completed = completed;
                row.updated_at = now;
                return Ok(row.clone());
            }

            let row = PlaybackProgress {
                id: Uuid::new_v4(),
                user_id: upsert.user_id,
                file_id: upsert.file_id,
                position_secs: upsert.position_secs,
                duration_secs: upsert.duration_secs,
                completed,
                updated_at: now,
            };
            rows.insert(row.id, row.clone());
            Ok(row)
        }

        async fn find_by_user_and_file(
            &self,
            user_id: Uuid,
            file_id: Uuid,
        ) -> Result<Option<PlaybackProgress>, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .find(|r| r.user_id == user_id && r.file_id == file_id)
                .cloned())
        }

        async fn find_in_progress_by_user(
            &self,
            user_id: Uuid,
            limit: u32,
        ) -> Result<Vec<PlaybackProgress>, DbErr> {
            let mut rows: Vec<PlaybackProgress> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id && !r.completed)
                .cloned()
                .collect();
            rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            rows.truncate(limit as usize);
            Ok(rows)
        }

        async fn find_page_by_user(
            &self,
            user_id: Uuid,
            limit: u64,
            offset: u64,
        ) -> Result<Vec<PlaybackProgress>, DbErr> {
            let mut rows: Vec<PlaybackProgress> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id)
                .cloned()
                .collect();
            rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            Ok(rows
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        }

        async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id)
                .count() as u64)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryPlaybackProgressRepository;

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::in_memory::InMemoryPlaybackProgressRepository;
    use crate::repositories::PlaybackProgressRepository;
    use crate::repositories::contract::fixture::PlaybackProgressFixture;
    use crate::services::TestClock;

    /// The hermetic instantiation of the shared contract. The in-memory store
    /// enforces no referential integrity, so a fresh v4 UUID is a valid
    /// identifier; the Postgres fixture in `beam-index` inserts real rows for
    /// the same calls.
    pub struct InMemoryFixture {
        repo: InMemoryPlaybackProgressRepository,
        clock: Arc<TestClock>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self::new()
        }
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                repo: InMemoryPlaybackProgressRepository::new(clock.clone()),
                clock,
            }
        }
    }

    #[async_trait::async_trait]
    impl PlaybackProgressFixture for InMemoryFixture {
        fn repo(&self) -> &dyn PlaybackProgressRepository {
            &self.repo
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }

        async fn new_user(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_file(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::new()
    }

    crate::playback_progress_repository_contract!(setup);
}
