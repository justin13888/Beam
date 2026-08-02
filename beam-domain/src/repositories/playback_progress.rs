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

#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryPlaybackProgressRepository {
        pub rows: Mutex<HashMap<Uuid, PlaybackProgress>>,
    }

    #[async_trait]
    impl PlaybackProgressRepository for InMemoryPlaybackProgressRepository {
        async fn upsert(&self, upsert: UpsertPlaybackProgress) -> Result<PlaybackProgress, DbErr> {
            let completed = upsert.is_completed();
            let mut rows = self.rows.lock().unwrap();
            let existing = rows
                .values_mut()
                .find(|r| r.user_id == upsert.user_id && r.file_id == upsert.file_id);

            let now = chrono::Utc::now();
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

#[cfg(test)]
mod tests {
    use super::in_memory::InMemoryPlaybackProgressRepository;
    use super::*;

    fn upsert(
        user_id: Uuid,
        file_id: Uuid,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> UpsertPlaybackProgress {
        UpsertPlaybackProgress {
            user_id,
            file_id,
            position_secs,
            duration_secs,
        }
    }

    #[tokio::test]
    async fn upsert_creates_then_updates_same_row() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();

        let first = repo
            .upsert(upsert(user_id, file_id, 10.0, Some(100.0)))
            .await
            .unwrap();
        let second = repo
            .upsert(upsert(user_id, file_id, 20.0, Some(100.0)))
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.position_secs, 20.0);
        assert_eq!(repo.rows.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn upsert_marks_completed_past_threshold() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();

        let row = repo
            .upsert(upsert(user_id, file_id, 98.0, Some(100.0)))
            .await
            .unwrap();
        assert!(row.completed);
    }

    #[tokio::test]
    async fn find_in_progress_by_user_excludes_completed_and_other_users() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let other_user = Uuid::new_v4();

        repo.upsert(upsert(user_id, Uuid::new_v4(), 10.0, Some(100.0)))
            .await
            .unwrap();
        repo.upsert(upsert(user_id, Uuid::new_v4(), 99.0, Some(100.0)))
            .await
            .unwrap(); // completed
        repo.upsert(upsert(other_user, Uuid::new_v4(), 10.0, Some(100.0)))
            .await
            .unwrap();

        let in_progress = repo.find_in_progress_by_user(user_id, 10).await.unwrap();
        assert_eq!(in_progress.len(), 1);
    }

    #[tokio::test]
    async fn find_in_progress_by_user_orders_most_recent_first() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let file_a = Uuid::new_v4();
        let file_b = Uuid::new_v4();

        repo.upsert(upsert(user_id, file_a, 10.0, Some(100.0)))
            .await
            .unwrap();
        // Ensure a distinguishable, later `updated_at` for file_b.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        repo.upsert(upsert(user_id, file_b, 10.0, Some(100.0)))
            .await
            .unwrap();

        let in_progress = repo.find_in_progress_by_user(user_id, 10).await.unwrap();
        assert_eq!(in_progress[0].file_id, file_b);
        assert_eq!(in_progress[1].file_id, file_a);
    }

    #[tokio::test]
    async fn find_in_progress_by_user_respects_limit() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        for _ in 0..5 {
            repo.upsert(upsert(user_id, Uuid::new_v4(), 10.0, Some(100.0)))
                .await
                .unwrap();
        }
        let in_progress = repo.find_in_progress_by_user(user_id, 2).await.unwrap();
        assert_eq!(in_progress.len(), 2);
    }

    #[tokio::test]
    async fn find_page_by_user_includes_completed_ordered_desc() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let file_a = Uuid::new_v4();
        let file_b = Uuid::new_v4();

        // `file_a` in-progress, `file_b` completed and updated later.
        repo.upsert(upsert(user_id, file_a, 10.0, Some(100.0)))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        repo.upsert(upsert(user_id, file_b, 99.0, Some(100.0)))
            .await
            .unwrap();

        let page = repo.find_page_by_user(user_id, 50, 0).await.unwrap();
        assert_eq!(page.len(), 2, "completed rows are included in history");
        assert_eq!(page[0].file_id, file_b, "most recent first");
        assert!(page[0].completed);
        assert_eq!(page[1].file_id, file_a);
    }

    #[tokio::test]
    async fn find_page_by_user_slices_by_limit_and_offset() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        for _ in 0..5 {
            repo.upsert(upsert(user_id, Uuid::new_v4(), 10.0, Some(100.0)))
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let page = repo.find_page_by_user(user_id, 2, 2).await.unwrap();
        assert_eq!(page.len(), 2);
    }

    #[tokio::test]
    async fn count_by_user_counts_all_rows_for_user_only() {
        let repo = InMemoryPlaybackProgressRepository::default();
        let user_id = Uuid::new_v4();
        let other_user = Uuid::new_v4();

        repo.upsert(upsert(user_id, Uuid::new_v4(), 10.0, Some(100.0)))
            .await
            .unwrap();
        repo.upsert(upsert(user_id, Uuid::new_v4(), 99.0, Some(100.0)))
            .await
            .unwrap(); // completed, still counted
        repo.upsert(upsert(other_user, Uuid::new_v4(), 10.0, Some(100.0)))
            .await
            .unwrap();

        assert_eq!(repo.count_by_user(user_id).await.unwrap(), 2);
    }
}
