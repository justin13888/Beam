use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieEntry, MovieSearchQuery};
use crate::providers::enrichment::MovieEnrichment;

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MovieRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Movie>, DbErr>;
    /// Server-side filtered/ranked search, replacing `find_all` + in-memory
    /// filtering for the browse/search API. Results are ordered
    /// best-match-first when `query.query` is set, else by title.
    async fn search(&self, query: &MovieSearchQuery) -> Result<Vec<Movie>, DbErr>;
    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr>;
    async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;
    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr>;
    /// Reverse lookup from a `MediaFileContent::Movie { movie_entry_id }` back
    /// to the entry (and, via `MovieEntry::movie_id`, the movie) -- used to
    /// resolve a file id to its movie for continue-watching.
    async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr>;
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        movie_id: Uuid,
    ) -> Result<(), DbErr>;
    /// Apply enrichment-provider data to an existing movie (title, year,
    /// description, external IDs, artwork, rating). Overwrites the current
    /// values -- enrichment is treated as the more authoritative source once
    /// a match is accepted.
    async fn apply_enrichment(
        &self,
        movie_id: Uuid,
        enrichment: &MovieEnrichment,
    ) -> Result<(), DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryMovieRepository {
        pub movies: Mutex<HashMap<Uuid, Movie>>,
        pub entries: Mutex<HashMap<Uuid, MovieEntry>>,
    }

    #[async_trait]
    impl MovieRepository for InMemoryMovieRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr> {
            Ok(self.movies.lock().unwrap().get(&id).cloned())
        }

        async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr> {
            Ok(self
                .movies
                .lock()
                .unwrap()
                .values()
                .find(|m| m.title == title)
                .cloned())
        }

        async fn find_all(&self) -> Result<Vec<Movie>, DbErr> {
            Ok(self.movies.lock().unwrap().values().cloned().collect())
        }

        async fn search(&self, query: &MovieSearchQuery) -> Result<Vec<Movie>, DbErr> {
            use crate::models::search::title_match_score;

            let mut scored: Vec<(f64, Movie)> = self
                .movies
                .lock()
                .unwrap()
                .values()
                .filter(|m| {
                    if query.year.is_some_and(|y| m.year != Some(y)) {
                        return false;
                    }
                    if query.year_from.is_some_and(|yf| m.year.unwrap_or(0) < yf) {
                        return false;
                    }
                    if query
                        .year_to
                        .is_some_and(|yt| m.year.unwrap_or(u32::MAX) > yt)
                    {
                        return false;
                    }
                    if let Some(min_r) = query.min_rating {
                        let rating = m.rating_tmdb.map(|r| (r * 10.0) as u32).unwrap_or(0);
                        if rating < min_r {
                            return false;
                        }
                    }
                    true
                })
                .filter_map(|m| {
                    let score = match &query.query {
                        Some(q) => title_match_score(&m.title, q),
                        None => 1.0,
                    };
                    (score > 0.0).then(|| (score, m.clone()))
                })
                .collect();

            scored.sort_by(|(a_score, a), (b_score, b)| {
                b_score
                    .partial_cmp(a_score)
                    .unwrap()
                    .then_with(|| a.title.cmp(&b.title))
            });
            Ok(scored.into_iter().map(|(_, m)| m).collect())
        }

        async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr> {
            let movie = Movie {
                id: Uuid::new_v4(),
                title: create.title,
                title_localized: None,
                description: None,
                year: create.year,
                release_date: None,
                runtime: create.runtime,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
                rating_tmdb: None,
                rating_imdb: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.movies.lock().unwrap().insert(movie.id, movie.clone());
            Ok(movie)
        }

        async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr> {
            let entry = MovieEntry {
                id: Uuid::new_v4(),
                library_id: create.library_id,
                movie_id: create.movie_id,
                edition: create.edition,
                is_primary: create.is_primary,
                created_at: chrono::Utc::now(),
            };
            self.entries.lock().unwrap().insert(entry.id, entry.clone());
            Ok(entry)
        }

        async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .values()
                .filter(|e| e.movie_id == movie_id)
                .cloned()
                .collect())
        }

        async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr> {
            Ok(self.entries.lock().unwrap().get(&entry_id).cloned())
        }

        async fn ensure_library_association(
            &self,
            _library_id: Uuid,
            _movie_id: Uuid,
        ) -> Result<(), DbErr> {
            Ok(())
        }

        async fn apply_enrichment(
            &self,
            movie_id: Uuid,
            enrichment: &MovieEnrichment,
        ) -> Result<(), DbErr> {
            let mut movies = self.movies.lock().unwrap();
            if let Some(movie) = movies.get_mut(&movie_id) {
                movie.title = enrichment.title.clone();
                movie.title_localized = enrichment.original_title.clone();
                movie.description = enrichment.description.clone();
                movie.year = enrichment.year;
                movie.release_date = enrichment.release_date;
                movie.poster_url = enrichment.poster_url.clone();
                movie.backdrop_url = enrichment.backdrop_url.clone();
                movie.tmdb_id = enrichment.tmdb_id;
                movie.imdb_id = enrichment.imdb_id.clone();
                movie.anilist_id = enrichment.anilist_id;
                movie.rating_tmdb = enrichment.rating;
                movie.updated_at = chrono::Utc::now();
            }
            Ok(())
        }
    }
}
