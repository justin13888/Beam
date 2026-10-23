use super::{ExternalIdentifiers, Ratings};

use super::Title;
use crate::models::playback::{UserGroupState, UserTitleState};
use chrono::NaiveDate;
use kynos::Schema;
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct ShowMetadata {
    /// Stable identifier for this show.
    pub id: Uuid,
    /// Title of the show
    pub title: Title,
    /// Optional description of the show
    pub description: Option<String>,
    /// Year the show was released
    pub year: Option<u32>,
    /// Path of the show's own poster image, served by the artwork route;
    /// absent when the show has no poster.
    pub poster_url: Option<String>,
    /// Path of the show's own backdrop image, served by the artwork route;
    /// absent when the show has no backdrop.
    pub backdrop_url: Option<String>,
    /// Genre names, sorted case-insensitively.
    pub genres: Vec<String>,
    /// The show's ratings.
    pub ratings: Option<Ratings>,
    /// The show's identifiers at external providers.
    pub identifiers: Option<ExternalIdentifiers>,
    /// How many seasons the show has.
    pub season_count: u32,
    /// How many episodes the show has, across every season.
    pub episode_count: u32,
    /// List of seasons in the show. Empty in a browse result, which carries
    /// `season_count` and `episode_count` instead; the detail route fills it.
    pub seasons: Vec<SeasonMetadata>,
    /// How much of the show the signed-in viewer has watched. Absent in a
    /// browse result; the detail route fills it.
    pub user_state: Option<UserGroupState>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SeasonMetadata {
    /// Season ID
    pub id: Uuid,
    /// Season number
    pub season_number: u32,
    /// Show dates
    pub dates: ShowDates,
    /// Runtime of episodes in whole minutes
    pub episode_runtime_mins: Option<u32>,
    /// List of episodes in the season
    pub episodes: Vec<EpisodeMetadata>,

    /// Optional URL to the season's poster image
    pub poster_url: Option<String>,

    /// Always empty: genres are recorded per show, on `ShowMetadata::genres`.
    pub genres: Vec<String>,
    /// Always absent: ratings are recorded per show, on
    /// `ShowMetadata::ratings`.
    pub ratings: Option<Ratings>,
    /// Always absent: identifiers are recorded per show, on
    /// `ShowMetadata::identifiers`.
    pub identifiers: Option<ExternalIdentifiers>,
    /// How much of the season the signed-in viewer has watched.
    pub user_state: Option<UserGroupState>,
    // Add people involved (cast, crew, directors, writers, etc.)
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct ShowDates {
    /// The day the season's first episode aired: a calendar date.
    pub first_aired: Option<NaiveDate>,
    /// The day the season's last episode aired: a calendar date.
    pub last_aired: Option<NaiveDate>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct EpisodeMetadata {
    /// Stable identifier for this episode.
    pub id: Uuid,
    /// Episode number within the season
    pub episode_number: u32,
    /// Title of the episode
    pub title: String,
    /// Optional description of the episode
    pub description: Option<String>,
    /// The day the episode aired: a calendar date.
    pub air_date: Option<NaiveDate>,
    /// Optional URL to the episode's thumbnail image
    pub thumbnail_url: Option<String>,

    /// Duration of the primary source's file in seconds. Absent when that
    /// file holds a run of episodes, whose duration is not this one's.
    pub duration_secs: Option<f64>,

    /// Identifier of the primary source's file -- the one
    /// `GET /v1/media/{id}/sources` lists first for this episode's id -- if
    /// the episode has any.
    pub file_id: Option<Uuid>,
    /// How many sources the episode has; their tracks are on
    /// `GET /v1/media/{id}/sources`.
    pub source_count: u32,
    /// The signed-in viewer's state for the episode.
    pub user_state: UserTitleState,
}

/// The show an episode or season belongs to, as its detail page heads it.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct ShowRef {
    pub id: Uuid,
    pub title: Title,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
}

/// What `GET /v1/episodes/{id}` returns: one episode, where it sits, and
/// its neighbours.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct EpisodeDetail {
    pub episode: EpisodeMetadata,
    pub season_id: Uuid,
    pub season_number: u32,
    pub show: ShowRef,
    /// The episode before this one in the show, by season then episode
    /// number -- across a season boundary, but never from a numbered season
    /// back into the specials (season 0) -- skipping episodes with no file
    /// to play; absent for the first.
    pub previous_episode_id: Option<Uuid>,
    /// The episode after this one, as next-up picks it: the first with a
    /// file to play, across a season boundary, after the run of episodes
    /// this one's primary file holds; absent for the last.
    pub next_episode_id: Option<Uuid>,
}

/// What `GET /v1/seasons/{id}` returns: one season with its episodes.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SeasonDetail {
    pub season: SeasonMetadata,
    pub show: ShowRef,
}
// TODO: detect discrepancy in video file length to detected episode length
