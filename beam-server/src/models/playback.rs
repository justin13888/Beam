//! Wire DTOs for watched state, continue-watching, and history (issue #188).
//!
//! Moved out of `services::playback` by the Kynos migration: ADR-0010 keeps the
//! service layer transport-independent, and a `Schema` derive is transport. The
//! service still owns the queries and imports these from here.

use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::search::{MediaTypeFilter, PageInfo};

/// The viewer's state for one movie or episode.
///
/// A title never played reads as all zeros: not played, at the start, no
/// plays, never played.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Schema)]
pub struct UserTitleState {
    /// Watched to the end at least once, or marked watched. Rewinding never
    /// clears it; marking the title unwatched does.
    pub played: bool,
    /// Where to resume, in seconds from the start: 0 for a title not started
    /// and for one just finished. A played title with a position is being
    /// rewatched.
    pub position_secs: f64,
    /// The duration `position_secs` was measured against, when known.
    pub duration_secs: Option<f64>,
    /// How many times the title was watched to the end or marked watched.
    pub play_count: u32,
    /// When the viewer last played or marked the title; absent if never.
    pub last_played_at: Option<DateTime<Utc>>,
    /// The source the viewer last played, to resume on it. Absent when none
    /// was played, or its file has since been removed.
    pub last_file_id: Option<Uuid>,
}

impl From<beam_domain::models::WatchState> for UserTitleState {
    fn from(state: beam_domain::models::WatchState) -> Self {
        let beam_domain::models::WatchState {
            id: _,
            user_id: _,
            target: _,
            last_file_id,
            position_secs,
            duration_secs,
            completed,
            play_count,
            last_played_at,
            dismissed_at: _,
        } = state;
        Self {
            played: completed,
            position_secs,
            duration_secs,
            play_count,
            last_played_at: Some(last_played_at),
            last_file_id,
        }
    }
}

/// The viewer's state for a season or a whole show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct UserGroupState {
    /// Every episode is played -- and there is at least one.
    pub played: bool,
    /// How many of the episodes are played.
    pub watched_episode_count: u32,
    /// How many episodes there are.
    pub episode_count: u32,
}

impl UserGroupState {
    /// The state of a group whose episodes' states are `episodes`.
    pub fn of<'a>(episodes: impl IntoIterator<Item = &'a UserTitleState>) -> Self {
        let (mut watched, mut total) = (0_u32, 0_u32);
        for episode in episodes {
            total = total.saturating_add(1);
            if episode.played {
                watched = watched.saturating_add(1);
            }
        }
        Self {
            played: total > 0 && watched == total,
            watched_episode_count: watched,
            episode_count: total,
        }
    }
}

/// Body of `PUT /v1/files/{file_id}/progress`.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct ReportProgressRequest {
    /// Where the viewer is, in seconds from the start of the file: at least
    /// 0, and no further than the end.
    pub position_secs: f64,
    /// How long the file is, in seconds, if the player knows; the probed
    /// duration is used otherwise. Positive.
    pub duration_secs: Option<f64>,
}

/// Why a continue-watching row is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ContinueWatchingReason {
    /// The viewer stopped part-way through this movie or episode.
    Resume,
    /// The viewer finished the episode before this one.
    NextUp,
}

/// One row of continue-watching: a movie to resume, or the one episode of a
/// show to watch next. Carries what a row displays, so a client renders it
/// without a request per row.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct ContinueWatchingItem {
    pub reason: ContinueWatchingReason,
    /// The movie or show, for its detail page.
    pub media_id: Uuid,
    pub media_type: MediaTypeFilter,
    /// The episode to play; absent for a movie.
    pub episode_id: Option<Uuid>,
    /// The source to play: the one the viewer last played if it is still
    /// present, else the title's primary source.
    pub file_id: Uuid,
    /// Where to start, in seconds: 0 for the next episode.
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    /// When the viewer last played anything of the title.
    pub last_played_at: DateTime<Utc>,
    /// The movie's or the show's title.
    pub title: String,
    /// The movie's or show's poster, else the season's.
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub episode_title: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// One row of watch history: a movie or an episode the viewer played or
/// marked watched, with what a row displays.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct HistoryItem {
    /// The movie or show, for its detail page.
    pub media_id: Uuid,
    pub media_type: MediaTypeFilter,
    /// The episode; absent for a movie.
    pub episode_id: Option<Uuid>,
    /// The source to play: the one last played if still present, else the
    /// primary.
    pub file_id: Uuid,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub played: bool,
    pub play_count: u32,
    pub last_played_at: DateTime<Utc>,
    pub title: String,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub episode_title: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// What `GET /v1/continue-watching` returns: at most `first` titles, most
/// recently played first -- one row per movie, one per show.
///
/// A shelf, not a paged list: it carries no cursors, and
/// `has_previous_page` is always `false`. A request examines at most the
/// 1,000 most recently played candidate titles; `has_next_page` is `true`
/// when candidates were left unexamined -- the shelf filled first, or that
/// ceiling was reached -- so more rows may exist than `items` holds.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct ContinueWatchingConnection {
    pub items: Vec<ContinueWatchingItem>,
    /// No cursors; `has_next_page` says whether more rows may exist.
    pub page_info: PageInfo,
}

/// A page of watch history, most recently played first.
///
/// Pass `page_info.end_cursor` as `after` for the next page. `total` counts
/// every title in the viewer's history; a title whose files are all gone is
/// counted but not listed, so a page can hold fewer than `first` items.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct HistoryConnection {
    pub items: Vec<HistoryItem>,
    pub page_info: PageInfo,
    pub total: u64,
}
