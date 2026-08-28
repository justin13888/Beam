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

/// Row counts per [`EnrichmentStatus`], backing the admin status endpoint's
/// queue overview (issue #85).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnrichmentStatusCounts {
    pub pending: u64,
    pub enriched: u64,
    pub unmatched: u64,
    pub failed: u64,
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

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn model(
        movie_id: Option<Uuid>,
        show_id: Option<Uuid>,
    ) -> beam_entity::metadata_enrichment::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = Utc::now().into();
        beam_entity::metadata_enrichment::Model {
            id: Uuid::new_v4(),
            movie_id,
            show_id,
            status: beam_entity::metadata_enrichment::EnrichmentStatus::Pending,
            attempts: 2,
            next_attempt_at: None,
            enriched_at: None,
            match_confidence: None,
            matched_ref: None,
            force_refresh: false,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_movie_row_becomes_a_movie_target() {
        let movie_id = Uuid::new_v4();
        let state = EnrichmentState::from(model(Some(movie_id), None));
        assert_eq!(state.target, EnrichmentTargetId::Movie(movie_id));
    }

    #[test]
    fn a_show_row_becomes_a_show_target() {
        // The two arms are near-identical and trivially transposable; getting
        // them the wrong way round would send every show through the movie
        // enrichment path.
        let show_id = Uuid::new_v4();
        let state = EnrichmentState::from(model(None, Some(show_id)));
        assert_eq!(state.target, EnrichmentTargetId::Show(show_id));
    }

    #[test]
    #[should_panic(expected = "exactly one of movie_id/show_id")]
    fn a_row_targeting_neither_is_a_broken_invariant_not_a_silent_default() {
        // The schema's check constraint makes this unreachable; if it ever is
        // reached, the row is corrupt and guessing a target would enrich the
        // wrong title.
        let _ = EnrichmentState::from(model(None, None));
    }

    #[test]
    #[should_panic(expected = "exactly one of movie_id/show_id")]
    fn a_row_targeting_both_is_a_broken_invariant_too() {
        let _ = EnrichmentState::from(model(Some(Uuid::new_v4()), Some(Uuid::new_v4())));
    }
}
