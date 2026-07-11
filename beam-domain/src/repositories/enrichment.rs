use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::enrichment::{EnrichmentState, EnrichmentStatusCounts, EnrichmentTargetId};

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

    /// Same as `request_refresh`, applied to every row. Returns the count affected.
    async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr>;

    /// Row counts grouped by status, for the admin status endpoint's queue
    /// overview (issue #85).
    async fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr>;
}

#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::enrichment::EnrichmentStatus;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    pub struct InMemoryEnrichmentStateRepository {
        rows: RwLock<HashMap<Uuid, EnrichmentState>>,
    }

    #[async_trait]
    impl EnrichmentStateRepository for InMemoryEnrichmentStateRepository {
        async fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr> {
            let mut rows = self.rows.write();
            if rows.values().any(|r| r.target == target) {
                return Ok(());
            }
            let id = Uuid::new_v4();
            rows.insert(
                id,
                EnrichmentState {
                    id,
                    target,
                    status: EnrichmentStatus::Pending,
                    attempts: 0,
                    next_attempt_at: None,
                    enriched_at: None,
                    match_confidence: None,
                    matched_ref: None,
                    force_refresh: false,
                    last_error: None,
                },
            );
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
            }
            Ok(())
        }

        async fn mark_unmatched(
            &self,
            id: Uuid,
            reason: &str,
            _now: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Unmatched;
                row.last_error = Some(reason.to_string());
                row.next_attempt_at = None;
                row.enriched_at = None;
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
            }
            Ok(())
        }

        async fn mark_failed(
            &self,
            id: Uuid,
            error: &str,
            _now: DateTime<Utc>,
        ) -> Result<(), DbErr> {
            if let Some(row) = self.rows.write().get_mut(&id) {
                row.status = EnrichmentStatus::Failed;
                row.last_error = Some(error.to_string());
                row.next_attempt_at = None;
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
                    row.status = EnrichmentStatus::Pending;
                    row.force_refresh = true;
                    row.attempts = 0;
                    row.next_attempt_at = None;
                    if rematch {
                        row.matched_ref = None;
                    }
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr> {
            let mut rows = self.rows.write();
            let mut count = 0u64;
            for row in rows.values_mut() {
                row.status = EnrichmentStatus::Pending;
                row.force_refresh = true;
                row.attempts = 0;
                row.next_attempt_at = None;
                if rematch {
                    row.matched_ref = None;
                }
                count += 1;
            }
            Ok(count)
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
