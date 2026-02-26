use chrono::{DateTime, Utc};
use std::time::Duration;
use uuid::Uuid;

/// A movie in the library
#[derive(Debug, Clone)]
pub struct Movie {
    pub id: Uuid,
    pub title: String,
    pub runtime: Option<Duration>,
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
            runtime: model
                .runtime_mins
                .map(|mins| Duration::from_secs((mins * 60) as u64)),
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
