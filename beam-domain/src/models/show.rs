use chrono::{DateTime, NaiveDate, Utc};
use std::time::Duration;
use uuid::Uuid;

/// A TV show in the library
#[derive(Debug, Clone)]
pub struct Show {
    pub id: Uuid,
    /// The display title: the series folder's parse until enrichment replaces
    /// it with the provider's.
    pub title: String,
    /// What the indexer matches an episode file's series folder to this show
    /// by -- see [`crate::utils::identity`]. Never rewritten by enrichment.
    /// `None` only on a row that predates the key and could not be
    /// backfilled, or that the indexer released (a show merged into another,
    /// or a season-folder husk); such a row is never matched.
    pub identity_key: Option<String>,
    /// The provider id a `tvshow.nfo` pins this show to (issue #184); see
    /// [`crate::models::movie::Movie::pinned_ref`].
    pub pinned_ref: Option<String>,
    /// Who set `pinned_ref`: `None` exactly when the title is not pinned.
    /// An NFO never replaces an administrator's pin (FR-312).
    pub pin_source: Option<crate::models::pin::PinSource>,
    pub title_localized: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub anilist_id: Option<u32>,
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

/// Parameters for finding or creating a show
#[derive(Debug, Clone)]
pub struct CreateShow {
    /// The show's identity; see [`crate::utils::identity`].
    pub identity_key: String,
    /// The version of the rules that derived `identity_key`; see
    /// [`crate::repositories::MovieRepository::find_keyed_before_version`].
    pub identity_key_version: u16,
    pub title: String,
    pub year: Option<u32>,
}

impl CreateShow {
    /// A show whose series folder parsed as `title` (and `year`), keyed by
    /// that parse.
    pub fn new(title: impl Into<String>, year: Option<u32>) -> Self {
        let title = title.into();
        Self {
            identity_key: crate::utils::identity::title_identity_key(&title, year),
            identity_key_version: crate::utils::media_path::CLASSIFIER_VERSION,
            title,
            year,
        }
    }
}

/// Server-side search/filter parameters for shows. See
/// [`crate::models::movie::MovieSearchQuery`] for the movie-side equivalent.
#[derive(Debug, Clone, Default)]
pub struct ShowSearchQuery {
    pub query: Option<String>,
    pub year: Option<u32>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
}

/// Parameters for creating an episode
#[derive(Debug, Clone)]
pub struct CreateEpisode {
    pub season_id: Uuid,
    pub episode_number: u32,
    pub title: String,
    pub runtime: Option<Duration>,
    /// When a date-based episode aired, read from its filename.
    pub air_date: Option<NaiveDate>,
}

#[cfg(feature = "entity")]
impl From<beam_entity::show::Model> for Show {
    fn from(model: beam_entity::show::Model) -> Self {
        Self {
            id: model.id,
            title: model.title,
            identity_key: model.identity_key,
            pinned_ref: model.pinned_ref,
            pin_source: model
                .pin_source
                .as_deref()
                .and_then(crate::models::pin::PinSource::parse),
            title_localized: model.title_localized,
            description: model.description,
            year: model.year.map(|y| y as u32),
            poster_url: model.poster_url,
            backdrop_url: model.backdrop_url,
            tmdb_id: model.tmdb_id.map(|id| id as u32),
            imdb_id: model.imdb_id,
            tvdb_id: model.tvdb_id.map(|id| id as u32),
            anilist_id: model.anilist_id.map(|id| id as u32),
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[cfg(feature = "entity")]
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

#[cfg(feature = "entity")]
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

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn episode_model(runtime_mins: Option<i32>) -> beam_entity::episode::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        beam_entity::episode::Model {
            id: Uuid::new_v4(),
            season_id: Uuid::new_v4(),
            episode_number: 2,
            title: "Half Loop".to_string(),
            description: None,
            air_date: None,
            runtime_mins,
            thumbnail_url: None,
            created_at: now,
        }
    }

    #[test]
    fn an_episode_runtime_in_minutes_becomes_a_duration_in_seconds() {
        assert_eq!(
            Episode::from(episode_model(Some(47))).runtime,
            Some(Duration::from_secs(47 * 60))
        );
        assert_eq!(
            Episode::from(episode_model(Some(1))).runtime,
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn an_unknown_episode_runtime_stays_unknown() {
        assert_eq!(Episode::from(episode_model(None)).runtime, None);
    }
}
