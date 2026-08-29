//! Wire DTOs for playback progress, continue-watching, and history.
//!
//! Moved out of `services::playback` by the Kynos migration: ADR-0010 keeps the
//! service layer transport-independent, and a `Schema` derive is transport. The
//! service still owns the queries and imports these from here.

use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackProgressDto {
    pub file_id: String,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

impl From<beam_domain::models::PlaybackProgress> for PlaybackProgressDto {
    fn from(p: beam_domain::models::PlaybackProgress) -> Self {
        Self {
            file_id: p.file_id.to_string(),
            position_secs: p.position_secs,
            duration_secs: p.duration_secs,
            completed: p.completed,
            updated_at: p.updated_at,
        }
    }
}

/// One row in the continue-watching list. `media_id`/`media_type` let the
/// client link to the detail page; `episode_id` is set only for episodes, so
/// the client can additionally deep-link to the right episode within a show.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct ContinueWatchingItem {
    pub file_id: String,
    pub media_id: String,
    pub media_type: String,
    pub episode_id: Option<String>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub updated_at: DateTime<Utc>,
}

/// One row in the watch-history list. Same resolved shape as
/// [`ContinueWatchingItem`] but additionally carries `completed`, since history
/// lists finished items too (continue-watching filters them out).
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct HistoryItem {
    pub file_id: String,
    pub media_id: String,
    pub media_type: String,
    pub episode_id: Option<String>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}
