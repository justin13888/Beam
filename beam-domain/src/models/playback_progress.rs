use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Resume/continue-watching state for a single (user, file) pair.
#[derive(Debug, Clone)]
pub struct PlaybackProgress {
    pub id: Uuid,
    pub user_id: Uuid,
    pub file_id: Uuid,
    pub position_secs: f64,
    /// Denormalized snapshot of the file's duration at time of last update,
    /// so "percent complete" can be computed without a join.
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

/// The fraction of `duration_secs` past which a report is considered
/// "completed" and drops off the continue-watching list.
pub const COMPLETED_THRESHOLD: f64 = 0.95;

/// Parameters for reporting/updating playback progress. `completed` is
/// derived server-side from `position_secs`/`duration_secs`, not accepted
/// from the caller.
#[derive(Debug, Clone)]
pub struct UpsertPlaybackProgress {
    pub user_id: Uuid,
    pub file_id: Uuid,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
}

impl UpsertPlaybackProgress {
    pub fn is_completed(&self) -> bool {
        self.duration_secs
            .is_some_and(|d| d > 0.0 && self.position_secs >= d * COMPLETED_THRESHOLD)
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::playback_progress::Model> for PlaybackProgress {
    fn from(model: beam_entity::playback_progress::Model) -> Self {
        Self {
            id: model.id,
            user_id: model.user_id,
            file_id: model.file_id,
            position_secs: model.position_secs,
            duration_secs: model.duration_secs,
            completed: model.completed,
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_completed_true_past_threshold() {
        let upsert = UpsertPlaybackProgress {
            user_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            position_secs: 96.0,
            duration_secs: Some(100.0),
        };
        assert!(upsert.is_completed());
    }

    #[test]
    fn is_completed_false_below_threshold() {
        let upsert = UpsertPlaybackProgress {
            user_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            position_secs: 50.0,
            duration_secs: Some(100.0),
        };
        assert!(!upsert.is_completed());
    }

    #[test]
    fn is_completed_false_when_duration_unknown() {
        let upsert = UpsertPlaybackProgress {
            user_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            position_secs: 1_000_000.0,
            duration_secs: None,
        };
        assert!(!upsert.is_completed());
    }
}
