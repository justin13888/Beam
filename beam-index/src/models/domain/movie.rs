use chrono::{DateTime, NaiveDate, Utc};
use std::time::Duration;
use uuid::Uuid;

/// A movie in the library
#[derive(Debug, Clone)]
pub struct Movie {
    pub id: Uuid,
    pub title: String,
    pub title_localized: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub release_date: Option<NaiveDate>,
    pub runtime: Option<Duration>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub rating_tmdb: Option<f32>,
    pub rating_imdb: Option<f32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A specific entry of a movie in a library (supports multiple editions)
#[derive(Debug, Clone)]
pub struct MovieEntry {
    pub id: Uuid,
    pub library_id: Uuid,
    pub movie_id: Uuid,
    pub edition: Option<String>,
    pub is_primary: bool,
    pub created_at: DateTime<Utc>,
}

/// Parameters for creating a movie
#[derive(Debug, Clone)]
pub struct CreateMovie {
    pub title: String,
    pub runtime: Option<Duration>,
}

/// Parameters for creating a movie entry
#[derive(Debug, Clone)]
pub struct CreateMovieEntry {
    pub library_id: Uuid,
    pub movie_id: Uuid,
    pub edition: Option<String>,
    pub is_primary: bool,
}

impl From<beam_entity::movie::Model> for Movie {
    fn from(model: beam_entity::movie::Model) -> Self {
        Self {
            id: model.id,
            title: model.title,
            title_localized: model.title_localized,
            description: model.description,
            year: model.year.map(|y| y as u32),
            release_date: model.release_date,
            runtime: model
                .runtime_mins
                .map(|mins| Duration::from_secs((mins * 60) as u64)),
            poster_url: model.poster_url,
            backdrop_url: model.backdrop_url,
            tmdb_id: model.tmdb_id.map(|id| id as u32),
            imdb_id: model.imdb_id,
            tvdb_id: model.tvdb_id.map(|id| id as u32),
            rating_tmdb: model.rating_tmdb,
            rating_imdb: model.rating_imdb,
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

impl From<beam_entity::movie_entry::Model> for MovieEntry {
    fn from(model: beam_entity::movie_entry::Model) -> Self {
        Self {
            id: model.id,
            library_id: model.library_id,
            movie_id: model.movie_id,
            edition: model.edition,
            is_primary: model.is_primary,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}
