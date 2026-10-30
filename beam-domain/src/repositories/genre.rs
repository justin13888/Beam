use std::collections::HashMap;

use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::access::AccessPolicy;
use crate::models::genre::Genre;

/// Genres, shared across movies/shows via junction tables and upserted by
/// slug so the same genre name reuses one row across titles.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait GenreRepository: Send + Sync + std::fmt::Debug {
    /// Upsert genres by slug and replace the movie's complete genre set with
    /// them (idempotent -- safe to call again with the same or updated names).
    async fn set_movie_genres(&self, movie_id: Uuid, names: &[String]) -> Result<(), DbErr>;
    /// Same as `set_movie_genres`, for shows.
    async fn set_show_genres(&self, show_id: Uuid, names: &[String]) -> Result<(), DbErr>;
    async fn find_all(&self) -> Result<Vec<Genre>, DbErr>;
    /// Every genre at least one movie or show `access` admits carries, in
    /// no particular order (issue #328): a genre only hidden titles carry,
    /// or none at all any more, is left out.
    async fn find_carried(&self, access: &AccessPolicy) -> Result<Vec<Genre>, DbErr>;
    /// The genre names of each of `movie_ids`, each list in
    /// [`crate::models::genre::sort_genre_names`] order. A movie with no genre is absent from the
    /// map. One statement however many ids, and none for an empty slice.
    async fn movie_genre_names(
        &self,
        movie_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, DbErr>;
    /// Same as `movie_genre_names`, for shows.
    async fn show_genre_names(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::genre::{slugify, sort_genre_names};
    use crate::models::watch_state::TitleRef;
    use crate::repositories::access_policy::in_memory::InMemoryTitleAccess;
    use parking_lot::RwLock;
    use std::collections::{HashMap, HashSet};

    /// The in-memory double. The `Default` applies access policies over no
    /// titles; see [`InMemoryTitleAccess`].
    #[derive(Debug, Default)]
    pub struct InMemoryGenreRepository {
        genres: RwLock<HashMap<String, Genre>>, // keyed by slug
        movie_genres: RwLock<HashMap<Uuid, Vec<Uuid>>>,
        show_genres: RwLock<HashMap<Uuid, Vec<Uuid>>>,
        titles: InMemoryTitleAccess,
    }

    impl InMemoryGenreRepository {
        /// A double applying access policies over the titles `titles` reads.
        pub fn with_titles(titles: InMemoryTitleAccess) -> Self {
            Self {
                titles,
                ..Self::default()
            }
        }

        fn upsert_genres(&self, names: &[String]) -> Vec<Uuid> {
            let mut genres = self.genres.write();
            names
                .iter()
                .map(|name| {
                    let slug = slugify(name);
                    genres
                        .entry(slug.clone())
                        .or_insert_with(|| Genre {
                            id: Uuid::new_v4(),
                            name: name.clone(),
                            slug,
                        })
                        .id
                })
                .collect()
        }

        pub fn genres_for_movie(&self, movie_id: Uuid) -> Vec<Genre> {
            let ids = self
                .movie_genres
                .read()
                .get(&movie_id)
                .cloned()
                .unwrap_or_default();
            let genres = self.genres.read();
            ids.iter()
                .filter_map(|id| genres.values().find(|g| g.id == *id).cloned())
                .collect()
        }

        pub fn genres_for_show(&self, show_id: Uuid) -> Vec<Genre> {
            let ids = self
                .show_genres
                .read()
                .get(&show_id)
                .cloned()
                .unwrap_or_default();
            let genres = self.genres.read();
            ids.iter()
                .filter_map(|id| genres.values().find(|g| g.id == *id).cloned())
                .collect()
        }
    }

    #[async_trait]
    impl GenreRepository for InMemoryGenreRepository {
        async fn set_movie_genres(&self, movie_id: Uuid, names: &[String]) -> Result<(), DbErr> {
            let ids = self.upsert_genres(names);
            self.movie_genres.write().insert(movie_id, ids);
            Ok(())
        }

        async fn set_show_genres(&self, show_id: Uuid, names: &[String]) -> Result<(), DbErr> {
            let ids = self.upsert_genres(names);
            self.show_genres.write().insert(show_id, ids);
            Ok(())
        }

        async fn find_all(&self) -> Result<Vec<Genre>, DbErr> {
            Ok(self.genres.read().values().cloned().collect())
        }

        async fn find_carried(&self, access: &AccessPolicy) -> Result<Vec<Genre>, DbErr> {
            let mut carried: HashSet<Uuid> = HashSet::new();
            for (movie_id, genre_ids) in self.movie_genres.read().iter() {
                if self.titles.admits(access, TitleRef::Movie(*movie_id)) {
                    carried.extend(genre_ids.iter().copied());
                }
            }
            for (show_id, genre_ids) in self.show_genres.read().iter() {
                if self.titles.admits(access, TitleRef::Show(*show_id)) {
                    carried.extend(genre_ids.iter().copied());
                }
            }
            Ok(self
                .genres
                .read()
                .values()
                .filter(|genre| carried.contains(&genre.id))
                .cloned()
                .collect())
        }

        async fn movie_genre_names(
            &self,
            movie_ids: &[Uuid],
        ) -> Result<HashMap<Uuid, Vec<String>>, DbErr> {
            Ok(names_of(movie_ids, |id| self.genres_for_movie(id)))
        }

        async fn show_genre_names(
            &self,
            show_ids: &[Uuid],
        ) -> Result<HashMap<Uuid, Vec<String>>, DbErr> {
            Ok(names_of(show_ids, |id| self.genres_for_show(id)))
        }
    }

    fn names_of(
        ids: &[Uuid],
        genres_of: impl Fn(Uuid) -> Vec<Genre>,
    ) -> HashMap<Uuid, Vec<String>> {
        ids.iter()
            .filter_map(|id| {
                let mut names: Vec<String> = genres_of(*id).into_iter().map(|g| g.name).collect();
                sort_genre_names(&mut names);
                (!names.is_empty()).then_some((*id, names))
            })
            .collect()
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::GenreRepository;
    use super::in_memory::InMemoryGenreRepository;
    use crate::repositories::access_policy::in_memory::InMemoryTitleAccess;
    use crate::repositories::contract::fixture::GenreRepositoryFixture;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::{MovieRepository, ShowRepository};

    /// The hermetic instantiation of the genre contract, the genre double
    /// reading the title doubles to apply a policy.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryGenreRepository,
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let movies = Arc::new(InMemoryMovieRepository::default());
            let shows = Arc::new(InMemoryShowRepository::default());
            Self {
                repo: InMemoryGenreRepository::with_titles(InMemoryTitleAccess::new(
                    movies.clone(),
                    shows.clone(),
                )),
                movies,
                shows,
            }
        }
    }

    #[async_trait::async_trait]
    impl GenreRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn GenreRepository {
            &self.repo
        }

        fn movies(&self) -> &dyn MovieRepository {
            self.movies.as_ref()
        }

        fn shows(&self) -> &dyn ShowRepository {
            self.shows.as_ref()
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::genre_repository_contract!(setup);
}
