//! Provider-agnostic metadata enrichment.
//!
//! Replaces the earlier `MetadataProvider` scaffold (never wired into any
//! service). No cameo (or any other external SDK) type appears here -- the
//! concrete cameo-backed adapter lives in beam-index, which is free to
//! change its dependency without touching this crate's public API.

use std::time::Duration;

use chrono::NaiveDate;
use thiserror::Error;

/// A canonical, provider-qualified external media identifier, e.g.
/// `"tmdb:603"` or `"anilist:5114"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalMediaRef(String);

impl ExternalMediaRef {
    pub fn new(provider: &str, native: &str) -> Self {
        Self(format!("{provider}:{native}"))
    }

    /// Parses a stored `"provider:id"` string. `None` if it has no `:`.
    pub fn parse(s: &str) -> Option<Self> {
        if s.contains(':') {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }

    pub fn provider(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    pub fn native(&self) -> &str {
        self.0.split_once(':').map(|x| x.1).unwrap_or("")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExternalMediaRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A query built from a parsed filename, used to search a provider.
#[derive(Debug, Clone)]
pub struct MediaQuery {
    pub title: String,
    pub year: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct MovieSearchHit {
    pub external_ref: ExternalMediaRef,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u32>,
    pub popularity: Option<f64>,
    pub vote_average: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ShowSearchHit {
    pub external_ref: ExternalMediaRef,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u32>,
    pub popularity: Option<f64>,
    pub vote_average: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MovieEnrichment {
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub anilist_id: Option<u32>,
    pub title: String,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub release_date: Option<NaiveDate>,
    pub runtime_mins: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ShowEnrichment {
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub anilist_id: Option<u32>,
    pub title: String,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub genres: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EpisodeEnrichment {
    pub episode_number: u32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub runtime_mins: Option<u32>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SeasonEnrichment {
    pub season_number: u32,
    pub poster_url: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub episodes: Vec<EpisodeEnrichment>,
}

#[derive(Debug, Error)]
pub enum EnrichmentError {
    #[error("not found")]
    NotFound,
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("no providers configured")]
    NotConfigured,
}

/// Abstracts external metadata lookups (TMDB/AniList via the `cameo` crate,
/// in production). Every method is fallible and must never panic on network
/// failures. Unit tests drive this exclusively through
/// [`test_utils::InMemoryEnrichmentProvider`] -- no test ever constructs a
/// real, network-capable implementation.
#[async_trait::async_trait]
pub trait EnrichmentProvider: Send + Sync + std::fmt::Debug {
    /// Configured upstream providers (e.g. `["tmdb", "anilist"]`). Empty
    /// means enrichment is effectively disabled.
    fn available_providers(&self) -> Vec<String>;

    async fn search_movies(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError>;
    async fn search_shows(&self, query: &MediaQuery)
    -> Result<Vec<ShowSearchHit>, EnrichmentError>;

    async fn movie_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError>;
    async fn show_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError>;
    async fn season_enrichment(
        &self,
        show: &ExternalMediaRef,
        season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError>;

    /// Best-effort: drop any cached response for `id` so the next fetch is fresh.
    async fn invalidate(&self, id: &ExternalMediaRef);
}

/// Production-safe default when no provider is configured at all.
#[derive(Debug, Default, Clone)]
pub struct NoopEnrichmentProvider;

#[async_trait::async_trait]
impl EnrichmentProvider for NoopEnrichmentProvider {
    fn available_providers(&self) -> Vec<String> {
        Vec::new()
    }

    async fn search_movies(
        &self,
        _query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn search_shows(
        &self,
        _query: &MediaQuery,
    ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn movie_enrichment(
        &self,
        _id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn show_enrichment(
        &self,
        _id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn season_enrichment(
        &self,
        _show: &ExternalMediaRef,
        _season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn invalidate(&self, _id: &ExternalMediaRef) {}
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn search_key(title: &str) -> String {
        title.to_lowercase()
    }

    /// Stateful, network-free fake driving every `EnrichmentProvider` unit
    /// test. Configure with the `with_*` builders (called before wrapping in
    /// `Arc`), then use normally through the trait.
    #[derive(Default)]
    pub struct InMemoryEnrichmentProvider {
        providers: Vec<String>,
        movie_search: HashMap<String, Vec<MovieSearchHit>>,
        show_search: HashMap<String, Vec<ShowSearchHit>>,
        movie_details: HashMap<String, MovieEnrichment>,
        show_details: HashMap<String, ShowEnrichment>,
        season_details: HashMap<(String, u32), SeasonEnrichment>,
        /// If set, every search call returns this error instead of a result
        /// (simulates rate-limiting/transport failures).
        search_error: Option<SharedError>,
        invalidated: Mutex<Vec<String>>,
    }

    /// `EnrichmentError` isn't `Clone` (it wraps a `Duration`, which is fine,
    /// but keeping the fake simple); store a constructor closure instead.
    type SharedError = std::sync::Arc<dyn Fn() -> EnrichmentError + Send + Sync>;

    impl InMemoryEnrichmentProvider {
        pub fn new(providers: &[&str]) -> Self {
            Self {
                providers: providers.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }

        pub fn with_movie_search(mut self, query_title: &str, hits: Vec<MovieSearchHit>) -> Self {
            self.movie_search.insert(search_key(query_title), hits);
            self
        }

        pub fn with_show_search(mut self, query_title: &str, hits: Vec<ShowSearchHit>) -> Self {
            self.show_search.insert(search_key(query_title), hits);
            self
        }

        pub fn with_movie_enrichment(mut self, enrichment: MovieEnrichment) -> Self {
            let key = enrichment
                .tmdb_id
                .map(|id| ExternalMediaRef::new("tmdb", &id.to_string()))
                .or_else(|| {
                    enrichment
                        .anilist_id
                        .map(|id| ExternalMediaRef::new("anilist", &id.to_string()))
                })
                .expect("movie enrichment fixture needs a tmdb_id or anilist_id");
            self.movie_details
                .insert(key.as_str().to_string(), enrichment);
            self
        }

        pub fn with_movie_enrichment_at(
            mut self,
            external_ref: ExternalMediaRef,
            enrichment: MovieEnrichment,
        ) -> Self {
            self.movie_details
                .insert(external_ref.as_str().to_string(), enrichment);
            self
        }

        pub fn with_show_enrichment(
            mut self,
            external_ref: ExternalMediaRef,
            enrichment: ShowEnrichment,
        ) -> Self {
            self.show_details
                .insert(external_ref.as_str().to_string(), enrichment);
            self
        }

        pub fn with_season_enrichment(
            mut self,
            show_ref: ExternalMediaRef,
            enrichment: SeasonEnrichment,
        ) -> Self {
            self.season_details.insert(
                (show_ref.as_str().to_string(), enrichment.season_number),
                enrichment,
            );
            self
        }

        /// Every subsequent search call fails with the given error.
        pub fn with_search_error(
            mut self,
            make_err: impl Fn() -> EnrichmentError + Send + Sync + 'static,
        ) -> Self {
            self.search_error = Some(std::sync::Arc::new(make_err));
            self
        }

        pub fn invalidated_refs(&self) -> Vec<String> {
            self.invalidated.lock().unwrap().clone()
        }
    }

    impl std::fmt::Debug for InMemoryEnrichmentProvider {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("InMemoryEnrichmentProvider")
                .field("providers", &self.providers)
                .field("movie_search", &self.movie_search)
                .field("show_search", &self.show_search)
                .field("movie_details", &self.movie_details)
                .field("show_details", &self.show_details)
                .field("season_details", &self.season_details)
                .field("search_error", &self.search_error.is_some())
                .field("invalidated", &self.invalidated)
                .finish()
        }
    }

    #[async_trait::async_trait]
    impl EnrichmentProvider for InMemoryEnrichmentProvider {
        fn available_providers(&self) -> Vec<String> {
            self.providers.clone()
        }

        async fn search_movies(
            &self,
            query: &MediaQuery,
        ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
            if let Some(err) = &self.search_error {
                return Err(err());
            }
            Ok(self
                .movie_search
                .get(&search_key(&query.title))
                .cloned()
                .unwrap_or_default())
        }

        async fn search_shows(
            &self,
            query: &MediaQuery,
        ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
            if let Some(err) = &self.search_error {
                return Err(err());
            }
            Ok(self
                .show_search
                .get(&search_key(&query.title))
                .cloned()
                .unwrap_or_default())
        }

        async fn movie_enrichment(
            &self,
            id: &ExternalMediaRef,
        ) -> Result<MovieEnrichment, EnrichmentError> {
            self.movie_details
                .get(id.as_str())
                .cloned()
                .ok_or(EnrichmentError::NotFound)
        }

        async fn show_enrichment(
            &self,
            id: &ExternalMediaRef,
        ) -> Result<ShowEnrichment, EnrichmentError> {
            self.show_details
                .get(id.as_str())
                .cloned()
                .ok_or(EnrichmentError::NotFound)
        }

        async fn season_enrichment(
            &self,
            show: &ExternalMediaRef,
            season_number: u32,
        ) -> Result<SeasonEnrichment, EnrichmentError> {
            self.season_details
                .get(&(show.as_str().to_string(), season_number))
                .cloned()
                .ok_or(EnrichmentError::NotFound)
        }

        async fn invalidate(&self, id: &ExternalMediaRef) {
            self.invalidated
                .lock()
                .unwrap()
                .push(id.as_str().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod external_media_ref {
        use super::*;

        #[test]
        fn a_reference_splits_into_provider_and_native_id() {
            let reference = ExternalMediaRef::parse("tmdb:603").expect("a valid reference");
            assert_eq!(reference.provider(), "tmdb");
            assert_eq!(reference.native(), "603");
            assert_eq!(reference.as_str(), "tmdb:603");
            // The canonical string is what is persisted in `matched_ref` and
            // parsed back on the next enrichment sweep, so it must round-trip.
            assert_eq!(reference.to_string(), "tmdb:603");
            assert_eq!(
                ExternalMediaRef::parse(&reference.to_string()),
                Some(reference)
            );
        }

        #[test]
        fn a_native_id_may_itself_contain_colons() {
            // Only the first colon separates provider from id; an id with its
            // own colons must survive intact.
            let reference = ExternalMediaRef::parse("custom:a:b:c").expect("valid");
            assert_eq!(reference.provider(), "custom");
            assert_eq!(reference.native(), "a:b:c");
        }

        #[test]
        fn a_string_without_a_colon_is_not_a_reference() {
            // Accepting it would produce a reference whose provider is the
            // whole string and whose id is empty -- a lookup that silently
            // matches nothing.
            assert_eq!(ExternalMediaRef::parse("603"), None);
            assert_eq!(ExternalMediaRef::parse(""), None);
        }

        #[test]
        fn empty_halves_are_still_reported_as_empty_rather_than_guessed_at() {
            let leading = ExternalMediaRef::parse(":603").expect("has a colon");
            assert_eq!(leading.provider(), "");
            assert_eq!(leading.native(), "603");

            let trailing = ExternalMediaRef::parse("tmdb:").expect("has a colon");
            assert_eq!(trailing.provider(), "tmdb");
            assert_eq!(trailing.native(), "");
        }
    }

    mod noop_provider {
        use super::*;

        fn query() -> MediaQuery {
            MediaQuery {
                title: "Arrival".to_string(),
                year: Some(2016),
            }
        }

        #[test]
        fn it_reports_no_configured_providers() {
            assert!(NoopEnrichmentProvider.available_providers().is_empty());
        }

        #[tokio::test]
        async fn every_lookup_says_not_configured_rather_than_no_results() {
            // The distinction matters: "no results" would let the enrichment
            // sweep mark titles `unmatched` and stop retrying them, so
            // configuring a provider later would never revisit them. "not
            // configured" is a transient failure the sweep leaves pending.
            let provider = NoopEnrichmentProvider;
            let reference = ExternalMediaRef::parse("tmdb:603").unwrap();

            assert!(matches!(
                provider.search_movies(&query()).await,
                Err(EnrichmentError::NotConfigured)
            ));
            assert!(matches!(
                provider.search_shows(&query()).await,
                Err(EnrichmentError::NotConfigured)
            ));
            assert!(matches!(
                provider.movie_enrichment(&reference).await,
                Err(EnrichmentError::NotConfigured)
            ));
            assert!(matches!(
                provider.show_enrichment(&reference).await,
                Err(EnrichmentError::NotConfigured)
            ));
            assert!(matches!(
                provider.season_enrichment(&reference, 1).await,
                Err(EnrichmentError::NotConfigured)
            ));
        }
    }
}
