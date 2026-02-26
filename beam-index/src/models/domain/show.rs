use chrono::{DateTime, NaiveDate, Utc};
use std::time::Duration;
use uuid::Uuid;

/// A TV show in the library
#[derive(Debug, Clone)]
pub struct Show {
    pub id: Uuid,
    pub title: String,
    pub title_localized: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A season within a TV show
#[derive(Debug, Clone)]
pub struct Season {
    pub id: Uuid,
    pub show_id: Uuid,
    pub season_number: u32,
    pub poster_url: Option<String>,
    pub first_aired: Option<NaiveDate>,
    pub last_aired: Option<NaiveDate>,
}

/// An episode within a season
#[derive(Debug, Clone)]
pub struct Episode {
    pub id: Uuid,
    pub season_id: Uuid,
    pub episode_number: u32,
    pub title: String,
    pub description: Option<String>,
    pub air_date: Option<String>,
    pub runtime: Option<Duration>,
    pub thumbnail_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Parameters for creating an episode
#[derive(Debug, Clone)]
pub struct CreateEpisode {
    pub season_id: Uuid,
    pub episode_number: u32,
    pub title: String,
    pub runtime: Option<Duration>,
}

impl From<beam_entity::show::Model> for Show {
    fn from(model: beam_entity::show::Model) -> Self {
        Self {
            id: model.id,
            title: model.title,
            title_localized: model.title_localized,
            description: model.description,
            year: model.year.map(|y| y as u32),
            poster_url: model.poster_url,
            backdrop_url: model.backdrop_url,
            tmdb_id: model.tmdb_id.map(|id| id as u32),
            imdb_id: model.imdb_id,
            tvdb_id: model.tvdb_id.map(|id| id as u32),
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

impl From<beam_entity::season::Model> for Season {
    fn from(model: beam_entity::season::Model) -> Self {
        Self {
            id: model.id,
            show_id: model.show_id,
            season_number: model.season_number as u32,
            poster_url: model.poster_url,
            first_aired: model.first_aired,
            last_aired: model.last_aired,
        }
    }
}

impl From<beam_entity::episode::Model> for Episode {
    fn from(model: beam_entity::episode::Model) -> Self {
        Self {
            id: model.id,
            season_id: model.season_id,
            episode_number: model.episode_number as u32,
            title: model.title,
            description: model.description,
            air_date: model.air_date.map(|d| d.to_string()),
            runtime: model
                .runtime_mins
                .map(|mins| Duration::from_secs((mins * 60) as u64)),
            thumbnail_url: model.thumbnail_url,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}
