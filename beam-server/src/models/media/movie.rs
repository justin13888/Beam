use super::{ExternalIdentifiers, Ratings, Title};
use crate::models::playback::UserTitleState;
use chrono::NaiveDate;
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
    /// The day the movie was released: a calendar date, with no time or zone.
    pub release_date: Option<NaiveDate>,
    /// Runtime of the movie in whole minutes, as its provider lists it.
    pub runtime_mins: Option<u32>,
    /// Duration of the primary source's file in seconds.
    pub duration_secs: Option<f64>,
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

    /// Identifier of the primary source's file -- the one
    /// `GET /v1/media/{id}/sources` lists first -- if the movie has any.
    /// Absent in a browse result, which does not read files.
    pub file_id: Option<Uuid>,
    /// How many sources the movie has. Absent in a browse result, which does
    /// not read files; their tracks are on `GET /v1/media/{id}/sources`.
    pub source_count: Option<u32>,
    /// The signed-in viewer's state for the movie.
    pub user_state: UserTitleState,
    //
    // TODO: Add people involved (cast, crew, directors, writers, etc.)
}
