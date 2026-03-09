use thiserror::Error;

/// Trait abstracting external metadata lookups (e.g., TMDb via the `cameo` crate).
///
/// All methods are async and fallible. Implementations should not panic on
/// network failures — return `MetadataProviderError` instead.
#[async_trait::async_trait]
pub trait MetadataProvider: Send + Sync + std::fmt::Debug {
    /// Search for movies by title, optionally filtering by release year.
    async fn search_movie(
        &self,
        title: &str,
        year: Option<u32>,
    ) -> Result<Vec<MovieMatch>, MetadataProviderError>;

    /// Search for TV shows by title.
    async fn search_show(&self, title: &str) -> Result<Vec<ShowMatch>, MetadataProviderError>;

    /// Get full movie details by TMDb ID.
    async fn get_movie(&self, tmdb_id: u32) -> Result<MovieDetail, MetadataProviderError>;

    /// Get full show details (with seasons and episodes) by TMDb ID.
    async fn get_show(&self, tmdb_id: u32) -> Result<ShowDetail, MetadataProviderError>;

    /// Resolve a TMDb image path to a full URL.
    ///
    /// `path` is the relative path returned by TMDb (e.g., "/abc123.jpg").
    /// `size` is a TMDb image size string (e.g., "w500", "original").
    fn image_url(&self, path: &str, size: &str) -> String;
}

// ── Search result types ─────────────────────────────────────────────────────

/// A movie search result from an external metadata provider.
#[derive(Debug, Clone)]
pub struct MovieMatch {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
}

/// A TV show search result from an external metadata provider.
#[derive(Debug, Clone)]
pub struct ShowMatch {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
}

// ── Detail types ────────────────────────────────────────────────────────────

/// Full movie metadata from an external provider.
#[derive(Debug, Clone)]
pub struct MovieDetail {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub title: String,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub release_date: Option<String>,
    pub runtime_mins: Option<u32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
}

/// Full TV show metadata from an external provider.
#[derive(Debug, Clone)]
pub struct ShowDetail {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub title: String,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub year: Option<u32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
    pub seasons: Vec<SeasonDetail>,
}

/// Season detail from an external provider.
#[derive(Debug, Clone)]
pub struct SeasonDetail {
    pub season_number: u32,
    pub name: Option<String>,
    pub description: Option<String>,
    pub poster_path: Option<String>,
    pub air_date: Option<String>,
    pub episodes: Vec<EpisodeDetail>,
}

/// Episode detail from an external provider.
#[derive(Debug, Clone)]
pub struct EpisodeDetail {
    pub episode_number: u32,
    pub title: String,
    pub description: Option<String>,
    pub air_date: Option<String>,
    pub runtime_mins: Option<u32>,
    pub still_path: Option<String>,
}

// ── Error ───────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MetadataProviderError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("network error: {0}")]
    NetworkError(String),
    #[error("rate limited")]
    RateLimited,
    #[error("provider error: {0}")]
    ProviderError(String),
}

// ── In-memory test implementation ───────────────────────────────────────────

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory metadata provider for unit tests.
    ///
    /// Pre-populate with `add_movie` / `add_show` before running test code.
    #[derive(Debug)]
    pub struct InMemoryMetadataProvider {
        movies: Mutex<HashMap<u32, MovieDetail>>,
        shows: Mutex<HashMap<u32, ShowDetail>>,
        image_base_url: String,
    }

    impl Default for InMemoryMetadataProvider {
        fn default() -> Self {
            Self {
                movies: Mutex::new(HashMap::new()),
                shows: Mutex::new(HashMap::new()),
                image_base_url: "https://image.tmdb.org/t/p".to_string(),
            }
        }
    }

    impl InMemoryMetadataProvider {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn add_movie(&self, detail: MovieDetail) {
            self.movies.lock().unwrap().insert(detail.tmdb_id, detail);
        }

        pub fn add_show(&self, detail: ShowDetail) {
            self.shows.lock().unwrap().insert(detail.tmdb_id, detail);
        }
    }

    #[async_trait::async_trait]
    impl MetadataProvider for InMemoryMetadataProvider {
        async fn search_movie(
            &self,
            title: &str,
            year: Option<u32>,
        ) -> Result<Vec<MovieMatch>, MetadataProviderError> {
            let movies = self.movies.lock().unwrap();
            let results: Vec<MovieMatch> = movies
                .values()
                .filter(|m| m.title.to_lowercase().contains(&title.to_lowercase()))
                .filter(|m| year.is_none() || m.year == year)
                .map(|m| MovieMatch {
                    tmdb_id: m.tmdb_id,
                    imdb_id: m.imdb_id.clone(),
                    title: m.title.clone(),
                    description: m.description.clone(),
                    year: m.year,
                    poster_path: m.poster_path.clone(),
                    backdrop_path: m.backdrop_path.clone(),
                    rating: m.rating,
                    genres: m.genres.clone(),
                })
                .collect();
            Ok(results)
        }

        async fn search_show(&self, title: &str) -> Result<Vec<ShowMatch>, MetadataProviderError> {
            let shows = self.shows.lock().unwrap();
            let results: Vec<ShowMatch> = shows
                .values()
                .filter(|s| s.title.to_lowercase().contains(&title.to_lowercase()))
                .map(|s| ShowMatch {
                    tmdb_id: s.tmdb_id,
                    imdb_id: s.imdb_id.clone(),
                    title: s.title.clone(),
                    description: s.description.clone(),
                    year: s.year,
                    poster_path: s.poster_path.clone(),
                    backdrop_path: s.backdrop_path.clone(),
                    rating: s.rating,
                    genres: s.genres.clone(),
                })
                .collect();
            Ok(results)
        }

        async fn get_movie(&self, tmdb_id: u32) -> Result<MovieDetail, MetadataProviderError> {
            self.movies
                .lock()
                .unwrap()
                .get(&tmdb_id)
                .cloned()
                .ok_or_else(|| MetadataProviderError::NotFound(format!("movie tmdb_id={tmdb_id}")))
        }

        async fn get_show(&self, tmdb_id: u32) -> Result<ShowDetail, MetadataProviderError> {
            self.shows
                .lock()
                .unwrap()
                .get(&tmdb_id)
                .cloned()
                .ok_or_else(|| MetadataProviderError::NotFound(format!("show tmdb_id={tmdb_id}")))
        }

        fn image_url(&self, path: &str, size: &str) -> String {
            format!("{}/{}{}", self.image_base_url, size, path)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn search_movie_by_title() {
            let provider = InMemoryMetadataProvider::new();
            provider.add_movie(MovieDetail {
                tmdb_id: 550,
                imdb_id: Some("tt0137523".to_string()),
                tvdb_id: None,
                title: "Fight Club".to_string(),
                original_title: None,
                description: Some("An insomniac office worker...".to_string()),
                year: Some(1999),
                release_date: Some("1999-10-15".to_string()),
                runtime_mins: Some(139),
                poster_path: Some("/pB8BM7pdSp6B6Ih7QZ4DrQ3PmJK.jpg".to_string()),
                backdrop_path: None,
                rating: Some(8.4),
                genres: vec!["Drama".to_string()],
            });

            let results = provider.search_movie("fight", None).await.unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].tmdb_id, 550);
            assert_eq!(results[0].title, "Fight Club");
        }

        #[tokio::test]
        async fn search_movie_with_year_filter() {
            let provider = InMemoryMetadataProvider::new();
            provider.add_movie(MovieDetail {
                tmdb_id: 550,
                imdb_id: None,
                tvdb_id: None,
                title: "Fight Club".to_string(),
                original_title: None,
                description: None,
                year: Some(1999),
                release_date: None,
                runtime_mins: None,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                genres: vec![],
            });

            let results = provider.search_movie("fight", Some(2000)).await.unwrap();
            assert!(results.is_empty());

            let results = provider.search_movie("fight", Some(1999)).await.unwrap();
            assert_eq!(results.len(), 1);
        }

        #[tokio::test]
        async fn get_movie_not_found() {
            let provider = InMemoryMetadataProvider::new();
            let result = provider.get_movie(999).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn search_show_by_title() {
            let provider = InMemoryMetadataProvider::new();
            provider.add_show(ShowDetail {
                tmdb_id: 1396,
                imdb_id: Some("tt0903747".to_string()),
                tvdb_id: None,
                title: "Breaking Bad".to_string(),
                original_title: None,
                description: None,
                year: Some(2008),
                poster_path: None,
                backdrop_path: None,
                rating: Some(9.5),
                genres: vec!["Drama".to_string(), "Crime".to_string()],
                seasons: vec![],
            });

            let results = provider.search_show("breaking").await.unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].tmdb_id, 1396);
        }

        #[tokio::test]
        async fn image_url_construction() {
            let provider = InMemoryMetadataProvider::new();
            let url = provider.image_url("/abc123.jpg", "w500");
            assert_eq!(url, "https://image.tmdb.org/t/p/w500/abc123.jpg");
        }
    }
}
