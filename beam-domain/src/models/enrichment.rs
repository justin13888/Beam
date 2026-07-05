use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Which title an enrichment row is for. Exactly one of movie/show, mirroring
/// the dual-nullable-FK-plus-CHECK pattern used by the `files` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnrichmentTargetId {
    Movie(Uuid),
    Show(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichmentStatus {
    Pending,
    Enriched,
    Unmatched,
    Failed,
}

/// Per-title enrichment queue/status row.
#[derive(Debug, Clone)]
pub struct EnrichmentState {
    pub id: Uuid,
    pub target: EnrichmentTargetId,
    pub status: EnrichmentStatus,
    pub attempts: u32,
    /// When this row becomes eligible for another attempt. `None` while
    /// `Pending` and never yet attempted.
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub enriched_at: Option<DateTime<Utc>>,
    pub match_confidence: Option<f32>,
    /// Canonical `"provider:id"` string, e.g. `"tmdb:603"`.
    pub matched_ref: Option<String>,
    pub force_refresh: bool,
    pub last_error: Option<String>,
}

#[cfg(feature = "entity")]
impl From<beam_entity::metadata_enrichment::EnrichmentStatus> for EnrichmentStatus {
    fn from(status: beam_entity::metadata_enrichment::EnrichmentStatus) -> Self {
        use beam_entity::metadata_enrichment::EnrichmentStatus as DbStatus;
        match status {
            DbStatus::Pending => EnrichmentStatus::Pending,
            DbStatus::Enriched => EnrichmentStatus::Enriched,
            DbStatus::Unmatched => EnrichmentStatus::Unmatched,
            DbStatus::Failed => EnrichmentStatus::Failed,
        }
    }
}

#[cfg(feature = "entity")]
impl From<EnrichmentStatus> for beam_entity::metadata_enrichment::EnrichmentStatus {
    fn from(status: EnrichmentStatus) -> Self {
        use beam_entity::metadata_enrichment::EnrichmentStatus as DbStatus;
        match status {
            EnrichmentStatus::Pending => DbStatus::Pending,
            EnrichmentStatus::Enriched => DbStatus::Enriched,
            EnrichmentStatus::Unmatched => DbStatus::Unmatched,
            EnrichmentStatus::Failed => DbStatus::Failed,
        }
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::metadata_enrichment::Model> for EnrichmentState {
    fn from(model: beam_entity::metadata_enrichment::Model) -> Self {
        let target = match (model.movie_id, model.show_id) {
            (Some(id), None) => EnrichmentTargetId::Movie(id),
            (None, Some(id)) => EnrichmentTargetId::Show(id),
            _ => unreachable!(
                "metadata_enrichment rows always have exactly one of movie_id/show_id set"
            ),
        };
        Self {
            id: model.id,
            target,
            status: model.status.into(),
            attempts: model.attempts as u32,
            next_attempt_at: model.next_attempt_at.map(|d| d.with_timezone(&Utc)),
            enriched_at: model.enriched_at.map(|d| d.with_timezone(&Utc)),
            match_confidence: model.match_confidence,
            matched_ref: model.matched_ref,
            force_refresh: model.force_refresh,
            last_error: model.last_error,
        }
    }
}
