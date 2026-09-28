use crate::models::MediaStreamMetadata;

use super::{ExternalIdentifiers, Ratings, Title};
use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct MovieMetadata {
    /// Stable identifier for this movie.
    pub id: Uuid,
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
    /// Genre names, sorted case-insensitively.
    pub genres: Vec<String>,
    /// Movie ratings
    pub ratings: Option<Ratings>,
    /// External identifiers to movie
    pub identifiers: Option<ExternalIdentifiers>,

    /// List of unique streams associated with this movie. Empty in a browse
    /// result, which does not read files; the detail route fills it.
    pub streams: Vec<MediaStreamMetadata>,
    /// Identifier of the primary streamable file, if any. Absent in a browse
    /// result, as `streams` is empty there.
    pub file_id: Option<Uuid>,
    //
    // TODO: Add people involved (cast, crew, directors, writers, etc.)
}
