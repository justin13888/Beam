use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
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
    ///
    /// Rows whose file is missing (`files.missing_since` set, issue #179) are
    /// excluded *before* the limit applies, so any number of them cannot
    /// push a visible row out of the list.
    async fn find_in_progress_by_user(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> Result<Vec<PlaybackProgress>, DbErr>;

    /// One page of a user's watch history, most-recently-updated first.
    /// Unlike [`find_in_progress_by_user`], this includes `completed` rows —
    /// the history view lists everything the user has watched. Rows whose
    /// file is missing are excluded before the page is sliced, exactly as in
    /// [`find_in_progress_by_user`].
    async fn find_page_by_user(
        &self,
        user_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlaybackProgress>, DbErr>;

    /// Total number of history rows for a user (completed and in-progress),
    /// for paginating [`find_page_by_user`]. Counts the same rows that method
    /// pages over, so rows whose file is missing are not counted.
    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr>;

    /// When each of `file_ids` was last played, by anyone: the latest
    /// `updated_at` among its progress rows, whether or not the file is
    /// missing. A file no one has played is absent from the map. The indexer
    /// breaks a tie between identical copies of a moved file with it (issue
    /// #180), so the row someone is watching keeps the file.
    async fn last_played_at(
        &self,
        file_ids: Vec<Uuid>,
    ) -> Result<HashMap<Uuid, DateTime<Utc>>, DbErr>;
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build. See
/// [`crate::services::clock::in_memory`] for why the `#[mutants::skip]` is
/// required.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::services::Clock;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    /// In-memory stand-in for the SQL repository.
    ///
    /// Takes the same [`Clock`] the real repository takes, so `updated_at`
    /// ordering is driven by an advanced [`crate::services::TestClock`] rather
    /// than by wall-clock time -- which is what lets the shared contract in
    /// [`super::contract`] assert ordering without sleeping.
    ///
    /// Takes the file store too: the list reads join `files` and drop rows
    /// whose file is missing, and the double answers that join from the same
    /// [`InMemoryFileRepository`] the caller marks files missing through.
    #[derive(Debug)]
    pub struct InMemoryPlaybackProgressRepository {
        rows: Mutex<HashMap<Uuid, PlaybackProgress>>,
        clock: Arc<dyn Clock>,
        files: Arc<InMemoryFileRepository>,
    }

    impl InMemoryPlaybackProgressRepository {
        pub fn new(clock: Arc<dyn Clock>, files: Arc<InMemoryFileRepository>) -> Self {
            Self {
                rows: Mutex::new(HashMap::new()),
                clock,
                files,
            }
        }

        /// The ids the SQL inner join on `files ... missing_since IS NULL`
        /// keeps: files that exist and are not missing.
        fn present_file_ids(&self) -> HashSet<Uuid> {
            self.files
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|file| file.missing_since.is_none())
                .map(|file| file.id)
                .collect()
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
            let present = self.present_file_ids();
            let mut rows: Vec<PlaybackProgress> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id && !r.completed && present.contains(&r.file_id))
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
            let present = self.present_file_ids();
            let mut rows: Vec<PlaybackProgress> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id && present.contains(&r.file_id))
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
            let present = self.present_file_ids();
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.user_id == user_id && present.contains(&r.file_id))
                .count() as u64)
        }

        async fn last_played_at(
            &self,
            file_ids: Vec<Uuid>,
        ) -> Result<HashMap<Uuid, DateTime<Utc>>, DbErr> {
            let mut last: HashMap<Uuid, DateTime<Utc>> = HashMap::new();
            for row in self.rows.lock().unwrap().values() {
                if file_ids.contains(&row.file_id) {
                    let latest = last.entry(row.file_id).or_insert(row.updated_at);
                    *latest = (*latest).max(row.updated_at);
                }
            }
            Ok(last)
        }
    }

    impl Default for InMemoryPlaybackProgressRepository {
        /// A store over a file store of its own, for a caller that never
        /// reads progress back through the missing-file join.
        fn default() -> Self {
            Self::new(
                Arc::new(crate::services::TestClock::new()),
                Arc::new(InMemoryFileRepository::default()),
            )
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
    use crate::models::file::{CreateMediaFile, FileStatus};
    use crate::repositories::contract::fixture::PlaybackProgressFixture;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::{FileRepository, PlaybackProgressRepository};
    use crate::services::{Clock, TestClock};

    /// The hermetic instantiation of the shared contract. The in-memory store
    /// enforces no referential integrity, so a fresh v4 UUID is a valid user;
    /// a file is a real row in the [`InMemoryFileRepository`] the progress
    /// double joins against. The Postgres fixture in `beam-index` inserts
    /// real rows for the same calls.
    pub struct InMemoryFixture {
        repo: InMemoryPlaybackProgressRepository,
        files: Arc<InMemoryFileRepository>,
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
            let files = Arc::new(InMemoryFileRepository::default());
            Self {
                repo: InMemoryPlaybackProgressRepository::new(clock.clone(), files.clone()),
                files,
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
            let id = Uuid::new_v4();
            self.files
                .create(CreateMediaFile {
                    library_id: Uuid::new_v4(),
                    path: std::path::PathBuf::from(format!("/videos/{id}.mkv")),
                    hash: 0,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status: FileStatus::Unknown,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await
                .expect("create a file row")
                .id
        }

        async fn mark_file_missing(&self, file_id: Uuid) {
            self.files
                .mark_missing(vec![file_id], self.clock.now())
                .await
                .expect("mark the file missing");
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
