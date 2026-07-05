use crate::models::MediaStreamMetadata;

use super::{ExternalIdentifiers, Ratings, Title};
use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct MovieMetadata {
    /// Stable identifier for this movie (UUID).
    pub id: String,
    /// Title of the movie
    pub title: Title,
    /// Optional description of the movie
    pub description: Option<String>,
    /// Year the movie was released
    pub year: Option<u32>,
    /// Release date of the movie
    pub release_date: Option<DateTime<Utc>>,
    /// Runtime of the movie in minutes
    pub runtime: Option<u32>,
    /// Duration of the video file in seconds
    pub duration: Option<f64>,
    /// Optional URL to the movie's poster image
    pub poster_url: Option<String>,
    /// Optional URL to the movie's backdrop image
    pub backdrop_url: Option<String>,
    /// Movie genres
    pub genres: Vec<String>, // TODO: Replace String with specific enum
    /// Movie ratings
    pub ratings: Option<Ratings>,
    /// External identifiers to movie
    pub identifiers: Option<ExternalIdentifiers>,

    /// List of unique streams associated with this movie
    pub streams: Vec<MediaStreamMetadata>,
    /// Identifier of the primary streamable file, if any.
    pub file_id: Option<String>,
    //
    // TODO: Add people involved (cast, crew, directors, writers, etc.)
}
