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
    pub anilist_id: Option<u32>,
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
    pub year: Option<u32>,
    pub runtime: Option<Duration>,
}

/// Server-side search/filter parameters for movies. `Sql*Repository` scores
/// `query` via Postgres `pg_trgm` similarity; `InMemory*Repository` uses
/// [`crate::models::search::title_match_score`] as an offline stand-in.
#[derive(Debug, Clone, Default)]
pub struct MovieSearchQuery {
    pub query: Option<String>,
    pub year: Option<u32>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
    /// Minimum rating on the same 0-100 scale as `rating_tmdb * 10`.
    pub min_rating: Option<u32>,
}

/// Parameters for creating a movie entry
#[derive(Debug, Clone)]
pub struct CreateMovieEntry {
    pub library_id: Uuid,
    pub movie_id: Uuid,
    pub edition: Option<String>,
    pub is_primary: bool,
}

#[cfg(feature = "entity")]
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
            anilist_id: model.anilist_id.map(|id| id as u32),
            rating_tmdb: model.rating_tmdb,
            rating_imdb: model.rating_imdb,
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[cfg(feature = "entity")]
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

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn model(runtime_mins: Option<i32>) -> beam_entity::movie::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        beam_entity::movie::Model {
            id: Uuid::new_v4(),
            title: "Arrival".to_string(),
            title_localized: None,
            description: None,
            year: Some(2016),
            release_date: None,
            runtime_mins,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: Some(329_865),
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            rating_imdb: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_runtime_in_minutes_becomes_a_duration_in_seconds() {
        // The stored column is minutes and the domain type is a `Duration`;
        // getting the conversion wrong shows a 116-minute film as under two
        // minutes (or over a hundred hours) in every UI that renders it.
        assert_eq!(
            Movie::from(model(Some(116))).runtime,
            Some(Duration::from_secs(116 * 60))
        );
        assert_eq!(
            Movie::from(model(Some(1))).runtime,
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn a_zero_runtime_is_a_zero_duration_not_an_absent_one() {
        assert_eq!(Movie::from(model(Some(0))).runtime, Some(Duration::ZERO));
    }

    #[test]
    fn an_unknown_runtime_stays_unknown() {
        assert_eq!(Movie::from(model(None)).runtime, None);
    }
}
