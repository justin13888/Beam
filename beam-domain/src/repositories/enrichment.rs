use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::enrichment::{
    EnrichmentListFilter, EnrichmentListQuery, EnrichmentState, EnrichmentStatusCounts,
    EnrichmentTargetId, FieldLocks,
};

/// Per-title enrichment queue/status, backing the `metadata_enrichment` table.
#[async_trait]
pub trait EnrichmentStateRepository: Send + Sync + std::fmt::Debug {
    /// Ensure a `Pending` row exists for `target` (no-op if one already
    /// exists, regardless of its current status).
    async fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr>;

    /// Create `Pending` rows for any movies/shows that don't have one yet.
    /// Returns the number of rows created. Intended as a one-time catch-up
    /// for titles indexed before enrichment existed; new titles get their
    /// row from `ensure_pending` at classification time instead.
    async fn backfill_missing(&self) -> Result<u64, DbErr>;

    /// Fetch up to `limit` rows that are due for (re-)enrichment as of `now`.
    async fn fetch_due(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<EnrichmentState>, DbErr>;

    /// The row for `target`, if it has one.
    async fn find_by_target(
        &self,
        target: EnrichmentTargetId,
    ) -> Result<Option<EnrichmentState>, DbErr>;

    async fn mark_enriched(
        &self,
        id: Uuid,
        matched_ref: &str,
        confidence: f32,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr>;

    async fn mark_unmatched(&self, id: Uuid, reason: &str, now: DateTime<Utc>)
    -> Result<(), DbErr>;

    /// Record a transient failure and schedule the next attempt. The row
    /// stays `Pending`; only exhausted attempts (decided by the caller)
    /// terminate into `mark_failed`.
    async fn mark_retrying(
        &self,
        id: Uuid,
        error: &str,
        attempts: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), DbErr>;

    /// Terminal failure: attempts exhausted.
    async fn mark_failed(&self, id: Uuid, error: &str, now: DateTime<Utc>) -> Result<(), DbErr>;

    /// Flip `target`'s row back to `Pending` so the worker re-processes it.
    /// `rematch` additionally clears the stored `matched_ref`, so the next
    /// pass re-searches rather than just re-fetching the same match.
    /// Returns `false` if no row exists for `target`.
    async fn request_refresh(
        &self,
        target: EnrichmentTargetId,
        rematch: bool,
    ) -> Result<bool, DbErr>;

    /// [`Self::request_refresh`] for every title associated with the library
    /// `library_id` (`MovieRepository::ensure_library_association`,
    /// `ShowRepository::ensure_library_association`), live or not; a title
    /// with no row yet gets a queued one, as [`Self::ensure_pending`] and
    /// then [`Self::request_refresh`] would give it. One statement, bound by
    /// the library's id alone, whatever the library's size. Returns how many
    /// titles it queued: every title of the library.
    async fn request_refresh_library(&self, library_id: Uuid, rematch: bool) -> Result<u64, DbErr>;

    /// Same as `request_refresh`, applied to every row, in one statement.
    /// Returns the count affected.
    async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr>;

    /// Lock exactly `locks` on `target` (issue #185), replacing whatever it
    /// had locked, and return the row. A title with no row yet gets a
    /// `Pending` one carrying the locks. Leaves the status and the match as
    /// they are: a lock changes what the next pass writes, not whether one
    /// runs.
    ///
    /// Atomic: concurrent calls for one title leave one row.
    async fn set_locked_fields(
        &self,
        target: EnrichmentTargetId,
        locks: &FieldLocks,
    ) -> Result<EnrichmentState, DbErr>;

    /// A page of the rows `query.filter` admits, most recently changed first
    /// (`updated_at` descending, then `id` descending), strictly after
    /// `query.after`, at most `query.limit` of them (FR-303).
    async fn list(&self, query: &EnrichmentListQuery) -> Result<Vec<EnrichmentState>, DbErr>;

    /// How many rows `filter` admits.
    async fn count(&self, filter: &EnrichmentListFilter) -> Result<u64, DbErr>;

    /// Row counts grouped by status, for the admin status endpoint's queue
    /// overview (issue #85).
    async fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::enrichment::EnrichmentStatus;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Debug, Default)]
    pub struct InMemoryEnrichmentStateRepository {
        rows: RwLock<HashMap<Uuid, EnrichmentState>>,
        /// The title stores whose library associations a library refresh
        /// reads; without them a library has no titles.
        titles: Option<Titles>,
    }

    #[derive(Debug)]
    struct Titles {
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
    }

    impl InMemoryEnrichmentStateRepository {
        /// A repository over the titles `movies` and `shows` hold, so that
        /// [`EnrichmentStateRepository::request_refresh_library`] finds a
        /// library's titles as a real store's join would.
        #[must_use]
        pub fn over_titles(
            movies: Arc<InMemoryMovieRepository>,
            shows: Arc<InMemoryShowRepository>,
        ) -> Self {
            Self {
                rows: RwLock::default(),
                titles: Some(Titles { movies, shows }),
            }
        }
    }

    fn pending(target: EnrichmentTargetId) -> EnrichmentState {
        EnrichmentState {
            id: Uuid::new_v4(),
            target,
            status: EnrichmentStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            enriched_at: None,
            match_confidence: None,
            matched_ref: None,
            force_refresh: false,
            last_error: None,
            locked_fields: FieldLocks::none(),
            updated_at: Utc::now(),
        }
    }

    fn queue(row: &mut EnrichmentState, rematch: bool) {
        row.status = EnrichmentStatus::Pending;
        row.force_refresh = true;
        row.attempts = 0;
        row.next_attempt_at = None;
        row.updated_at = Utc::now();
        if rematch {
            row.matched_ref = None;
        }
    }

    #[async_trait]
    impl EnrichmentStateRepository for InMemoryEnrichmentStateRepository {
        async fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr> {
            let mut rows = self.rows.write();
            if rows.values().any(|r| r.target == target) {
                return Ok(());
            }
            let row = pending(target);
            rows.insert(row.id, row);
            Ok(())
        }

        async fn backfill_missing(&self) -> Result<u64, DbErr> {
            // The in-memory fake has no separate movie/show table to backfill
            // from; tests seed rows directly via `ensure_pending`.
            Ok(0)
        }

        async fn fetch_due(
            &self,
            now: DateTime<Utc>,
            limit: u32,
        ) -> Result<Vec<EnrichmentState>, DbErr> {
            let rows = self.rows.read();
            let mut due: Vec<EnrichmentState> = rows
                .values()
                .filter(|r| r.status == EnrichmentStatus::Pending)
                .filter(|r| r.next_attempt_at.is_none_or(|t| t <= now))
                .cloned()
                .collect();
            due.sort_by_key(|r| r.next_attempt_at);
            due.truncate(limit as usize);
            Ok(due)
        }

        async fn find_by_target(
            &self,
            target: EnrichmentTargetId,
        ) -> Result<Option<EnrichmentState>, DbErr> {
            Ok(self
                .rows
                .read()
                .values()
                .find(|r| r.target == target)
                .cloned())
        }

        async fn mark_enriched(
            &self,
            id: Uuid,
            matched_ref: &str,
            confidence: f32,
            now: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Enriched;
                row.matched_ref = Some(matched_ref.to_string());
                row.match_confidence = Some(confidence);
                row.enriched_at = Some(now);
                row.next_attempt_at = None;
                row.force_refresh = false;
                row.last_error = None;
                row.updated_at = now;
            }
            Ok(())
        }

        async fn mark_unmatched(
            &self,
            id: Uuid,
            reason: &str,
            now: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Unmatched;
                row.last_error = Some(reason.to_string());
                row.next_attempt_at = None;
                row.enriched_at = None;
                row.updated_at = now;
            }
            Ok(())
        }

        async fn mark_retrying(
            &self,
            id: Uuid,
            error: &str,
            attempts: u32,
            next_attempt_at: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Pending;
                row.attempts = attempts;
                row.next_attempt_at = Some(next_attempt_at);
                row.last_error = Some(error.to_string());
                row.updated_at = Utc::now();
            }
            Ok(())
        }

        async fn mark_failed(
            &self,
            id: Uuid,
            error: &str,
            now: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Failed;
                row.last_error = Some(error.to_string());
                row.next_attempt_at = None;
                row.updated_at = now;
            }
            Ok(())
        }

        async fn request_refresh(
            &self,
            target: EnrichmentTargetId,
            rematch: bool,
        ) -> Result<bool, DbErr> {
            let mut rows = self.rows.write();
            match rows.values_mut().find(|r| r.target == target) {
                Some(row) => {
                    queue(row, rematch);
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn request_refresh_library(
            &self,
            library_id: Uuid,
            rematch: bool,
        ) -> Result<u64, DbErr> {
            let Some(titles) = &self.titles else {
                return Ok(0);
            };
            let targets: Vec<EnrichmentTargetId> = titles
                .movies
                .ids_in_library(library_id)
                .into_iter()
                .map(EnrichmentTargetId::Movie)
                .chain(
                    titles
                        .shows
                        .ids_in_library(library_id)
                        .into_iter()
                        .map(EnrichmentTargetId::Show),
                )
                .collect();
            let mut rows = self.rows.write();
            for &target in &targets {
                if !rows.values().any(|r| r.target == target) {
                    let row = pending(target);
                    rows.insert(row.id, row);
                }
            }
            for row in rows.values_mut().filter(|r| targets.contains(&r.target)) {
                queue(row, rematch);
            }
            Ok(targets.len() as u64)
        }

        async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr> {
            let mut rows = self.rows.write();
            let mut count = 0u64;
            for row in rows.values_mut() {
                queue(row, rematch);
                count += 1;
            }
            Ok(count)
        }

        async fn set_locked_fields(
            &self,
            target: EnrichmentTargetId,
            locks: &FieldLocks,
        ) -> Result<EnrichmentState, DbErr> {
            let mut rows = self.rows.write();
            let id = match rows.values().find(|r| r.target == target) {
                Some(row) => row.id,
                None => {
                    let row = pending(target);
                    let id = row.id;
                    rows.insert(id, row);
                    id
                }
            };
            let row = rows.get_mut(&id).expect("the row was just found or made");
            row.locked_fields = locks.clone();
            row.updated_at = Utc::now();
            Ok(row.clone())
        }

        async fn list(&self, query: &EnrichmentListQuery) -> Result<Vec<EnrichmentState>, DbErr> {
            let rows = self.rows.read();
            let mut listed: Vec<EnrichmentState> = rows
                .values()
                .filter(|r| query.filter.admits(r))
                .filter(|r| query.after.is_none_or(|after| r.list_position() < after))
                .cloned()
                .collect();
            listed.sort_by_key(|r| std::cmp::Reverse(r.list_position()));
            listed.truncate(query.limit.get() as usize);
            Ok(listed)
        }

        async fn count(&self, filter: &EnrichmentListFilter) -> Result<u64, DbErr> {
            Ok(self
                .rows
                .read()
                .values()
                .filter(|r| filter.admits(r))
                .count() as u64)
        }

        async fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr> {
            let rows = self.rows.read();
            let mut counts = EnrichmentStatusCounts::default();
            for row in rows.values() {
                match row.status {
                    EnrichmentStatus::Pending => counts.pending += 1,
                    EnrichmentStatus::Enriched => counts.enriched += 1,
                    EnrichmentStatus::Unmatched => counts.unmatched += 1,
                    EnrichmentStatus::Failed => counts.failed += 1,
                }
            }
            Ok(counts)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use super::EnrichmentStateRepository;
    use super::in_memory::InMemoryEnrichmentStateRepository;
    use crate::repositories::contract::fixture::EnrichmentStateFixture;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::{MovieRepository, ShowRepository};
    use std::sync::Arc;
    use uuid::Uuid;

    /// The hermetic instantiation of the enrichment-state contract.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryEnrichmentStateRepository,
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let movies = Arc::new(InMemoryMovieRepository::default());
            let shows = Arc::new(InMemoryShowRepository::default());
            Self {
                repo: InMemoryEnrichmentStateRepository::over_titles(movies.clone(), shows.clone()),
                movies,
                shows,
            }
        }
    }

    #[async_trait::async_trait]
    impl EnrichmentStateFixture for InMemoryFixture {
        fn repo(&self) -> &dyn EnrichmentStateRepository {
            &self.repo
        }

        fn movies(&self) -> &dyn MovieRepository {
            self.movies.as_ref()
        }

        fn shows(&self) -> &dyn ShowRepository {
            self.shows.as_ref()
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

    crate::enrichment_state_repository_contract!(setup);
}
