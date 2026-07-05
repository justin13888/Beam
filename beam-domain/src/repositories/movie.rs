use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};
use crate::providers::enrichment::MovieEnrichment;

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MovieRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr>;
    async fn find_by_title(&self, title: &str) -> Result<Option<Movie>, DbErr>;
    async fn find_all(&self) -> Result<Vec<Movie>, DbErr>;
    async fn create(&self, create: CreateMovie) -> Result<Movie, DbErr>;
    async fn create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;
    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr>;
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
