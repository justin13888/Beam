use chrono::{DateTime, Utc};
use std::time::Duration;
use uuid::Uuid;

/// A TV show in the library
#[derive(Debug, Clone)]
pub struct Show {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A season within a TV show
#[derive(Debug, Clone)]
pub struct Season {
    pub id: Uuid,
    pub show_id: Uuid,
    pub season_number: u32,
}

/// An episode within a season
#[derive(Debug, Clone)]
pub struct Episode {
    pub id: Uuid,
    pub season_id: Uuid,
    pub episode_number: u32,
    pub title: String,
    pub runtime: Option<Duration>,
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
            runtime: model
                .runtime_mins
                .map(|mins| Duration::from_secs((mins * 60) as u64)),
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}
