use crate::models::MediaStreamMetadata;

use super::{ExternalIdentifiers, Ratings};

use super::Title;
use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct ShowMetadata {
    /// Stable identifier for this show (UUID).
    pub id: String,
    /// Title of the show
    pub title: Title,
    /// Optional description of the show
    pub description: Option<String>,
    /// Year the show was released
    pub year: Option<u32>,
    /// List of seasons in the show
    pub seasons: Vec<SeasonMetadata>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct SeasonMetadata {
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

    /// Show genres
    pub genres: Vec<String>, // TODO: Replace String with specific enum
    /// Show ratings
    pub ratings: Option<Ratings>,
    /// External identifiers to show
    pub identifiers: Option<ExternalIdentifiers>,
    // Add people involved (cast, crew, directors, writers, etc.)
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct ShowDates {
    /// First air date
    pub first_aired: Option<DateTime<Utc>>,
    /// Last air date
    pub last_aired: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct EpisodeMetadata {
    /// Stable identifier for this episode (UUID).
    pub id: String,
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
    pub file_id: Option<String>,
}
// TODO: detect discrepancy in video file length to detected episode length
