use crate::models::MediaStreamMetadata;

use super::{ExternalIdentifiers, Ratings};

use super::Title;
use chrono::{DateTime, Utc};
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
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SeasonMetadata {
    /// Season ID
    pub id: Uuid,
    /// Season number
    pub season_number: u32,
    /// Show dates
    pub dates: ShowDates,
    /// Runtime of episodes in minutes
    pub episode_runtime: Option<u32>,
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
    // Add people involved (cast, crew, directors, writers, etc.)
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct ShowDates {
    /// First air date
    pub first_aired: Option<DateTime<Utc>>,
    /// Last air date
    pub last_aired: Option<DateTime<Utc>>,
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
    /// Optional air date of the episode in YYYY-MM-DD format
    pub air_date: Option<String>,
    /// Optional URL to the episode's thumbnail image
    pub thumbnail_url: Option<String>,

    pub duration: Option<f64>,

    /// List of unique streams associated with this episode
    pub streams: Vec<MediaStreamMetadata>,
    /// Identifier of the streamable file backing this episode, if any.
    pub file_id: Option<Uuid>,
}
// TODO: detect discrepancy in video file length to detected episode length
