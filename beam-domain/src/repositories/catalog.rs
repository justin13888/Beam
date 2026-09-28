use async_trait::async_trait;
use sea_orm::DbErr;

use crate::models::catalog::{CatalogPosition, CatalogQuery};

/// The catalogue read model: browse and search over movies and shows together
/// (issue #187).
///
/// Only **live** titles are listed -- a movie with a present file, a show with
/// an episode that has one (issues #179, #183). A title whose files are all
/// missing, or that never had one, is absent from every page and every filter,
/// though a read by id through its own repository still resolves it.
///
/// A page is ordered by `query.sort` with the `(kind, id)` tie-break, a missing
/// value after every present one in both directions (see
/// [`CatalogPosition::display_cmp`]). It is returned in that display order
/// whichever way `query.seek` runs, and holds at most `query.limit` rows: the
/// first ones after the seek position, or the last ones before it. The seek
/// position need not be a listed title any more -- a page after a title that
/// has since left the listing continues exactly where it was.
///
/// A seek position whose key is not a value of `query.sort.field` is refused.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait CatalogRepository: Send + Sync + std::fmt::Debug {
    /// One page of positions; the caller reads the titles themselves by id.
    async fn browse(&self, query: &CatalogQuery) -> Result<Vec<CatalogPosition>, DbErr>;
}

/// The error a position keyed for another field is refused with.
#[must_use]
pub fn mismatched_position(query: &CatalogQuery) -> Option<DbErr> {
    let position = query.position()?;
    (position.key.field() != query.sort.field).then(|| {
        DbErr::Custom(format!(
            "a {:?} position cannot seek a listing sorted by {:?}",
            position.key.field(),
            query.sort.field
        ))
    })
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use std::sync::Arc;

    use super::*;
    use crate::models::catalog::{CatalogFilters, CatalogSortField, Seek, SortKey, TitleKind};
    use crate::models::search::title_match_score;
    use crate::repositories::genre::in_memory::InMemoryGenreRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;

    /// The in-memory double, reading the movie, show and genre doubles a test
    /// writes through. Liveness comes from the file double those title doubles
    /// are linked to; an unlinked title double treats every title as live.
    #[derive(Debug)]
    pub struct InMemoryCatalogRepository {
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        genres: Arc<InMemoryGenreRepository>,
    }

    impl InMemoryCatalogRepository {
        pub fn new(
            movies: Arc<InMemoryMovieRepository>,
            shows: Arc<InMemoryShowRepository>,
            genres: Arc<InMemoryGenreRepository>,
        ) -> Self {
            Self {
                movies,
                shows,
                genres,
            }
        }
    }

    /// The fields a filter or a sort reads, for either kind.
    struct Candidate {
        kind: TitleKind,
        id: uuid::Uuid,
        title: String,
        year: Option<u32>,
        rating: Option<f32>,
        created_at: chrono::DateTime<chrono::Utc>,
        runtime_mins: Option<i32>,
        genre_slugs: Vec<String>,
    }

    fn admitted(candidate: &Candidate, filters: &CatalogFilters) -> bool {
        let CatalogFilters {
            kind,
            query,
            genre_slug,
            year,
            year_from,
            year_to,
            min_rating,
        } = filters;
        kind.is_none_or(|kind| kind == candidate.kind)
            && query
                .as_deref()
                .is_none_or(|q| title_match_score(&candidate.title, q) > 0.0)
            && genre_slug
                .as_deref()
                .is_none_or(|slug| candidate.genre_slugs.iter().any(|s| s == slug))
            && year.is_none_or(|y| candidate.year == Some(y))
            && year_from.is_none_or(|y| candidate.year.is_some_and(|c| c >= y))
            && year_to.is_none_or(|y| candidate.year.is_some_and(|c| c <= y))
            && min_rating.is_none_or(|min| candidate.rating.map_or(0.0, |r| r * 10.0) >= min as f32)
    }

    fn key(candidate: &Candidate, field: CatalogSortField) -> SortKey {
        match field {
            CatalogSortField::Title => SortKey::Title(candidate.title.to_lowercase()),
            CatalogSortField::Year => SortKey::Year(candidate.year.map(|y| y as i32)),
            CatalogSortField::Rating => SortKey::Rating(candidate.rating),
            CatalogSortField::DateAdded => SortKey::DateAdded(candidate.created_at),
            CatalogSortField::Runtime => SortKey::Runtime(candidate.runtime_mins),
        }
    }

    #[async_trait]
    impl CatalogRepository for InMemoryCatalogRepository {
        async fn browse(&self, query: &CatalogQuery) -> Result<Vec<CatalogPosition>, DbErr> {
            if let Some(err) = mismatched_position(query) {
                return Err(err);
            }

            let live_movies = self.movies.live_movie_ids();
            let live_shows = self.shows.live_show_ids();
            let mut candidates: Vec<Candidate> = Vec::new();
            for movie in self.movies.movies.lock().unwrap().values() {
                if live_movies
                    .as_ref()
                    .is_some_and(|live| !live.contains(&movie.id))
                {
                    continue;
                }
                candidates.push(Candidate {
                    kind: TitleKind::Movie,
                    id: movie.id,
                    title: movie.title.clone(),
                    year: movie.year,
                    rating: movie.rating_tmdb,
                    created_at: movie.created_at,
                    runtime_mins: movie.runtime.map(|d| (d.as_secs() / 60) as i32),
                    genre_slugs: self
                        .genres
                        .genres_for_movie(movie.id)
                        .into_iter()
                        .map(|g| g.slug)
                        .collect(),
                });
            }
            for show in self.shows.shows.lock().unwrap().values() {
                if live_shows
                    .as_ref()
                    .is_some_and(|live| !live.contains(&show.id))
                {
                    continue;
                }
                candidates.push(Candidate {
                    kind: TitleKind::Show,
                    id: show.id,
                    title: show.title.clone(),
                    year: show.year,
                    rating: show.rating_tmdb,
                    created_at: show.created_at,
                    runtime_mins: None,
                    genre_slugs: self
                        .genres
                        .genres_for_show(show.id)
                        .into_iter()
                        .map(|g| g.slug)
                        .collect(),
                });
            }

            let direction = query.sort.direction;
            let mut positions: Vec<CatalogPosition> = candidates
                .iter()
                .filter(|c| admitted(c, &query.filters))
                .map(|c| CatalogPosition {
                    kind: c.kind,
                    id: c.id,
                    key: key(c, query.sort.field),
                })
                .collect();
            positions.sort_by(|a, b| a.display_cmp(b, direction));

            let limit = query.limit.get() as usize;
            Ok(match &query.seek {
                Seek::Forward(after) => positions
                    .into_iter()
                    .filter(|p| {
                        after
                            .as_ref()
                            .is_none_or(|after| p.display_cmp(after, direction).is_gt())
                    })
                    .take(limit)
                    .collect(),
                Seek::Backward(before) => {
                    let earlier: Vec<CatalogPosition> = positions
                        .into_iter()
                        .filter(|p| {
                            before
                                .as_ref()
                                .is_none_or(|before| p.display_cmp(before, direction).is_lt())
                        })
                        .collect();
                    let skip = earlier.len().saturating_sub(limit);
                    earlier.into_iter().skip(skip).collect()
                }
            })
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::CatalogRepository;
    use super::in_memory::InMemoryCatalogRepository;
    use crate::repositories::contract::fixture::CatalogRepositoryFixture;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::genre::in_memory::InMemoryGenreRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::{FileRepository, GenreRepository, MovieRepository, ShowRepository};

    /// The hermetic instantiation of the catalogue contract: every double
    /// linked to one file double.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryCatalogRepository,
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        genres: Arc<InMemoryGenreRepository>,
        files: Arc<InMemoryFileRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let files = Arc::new(InMemoryFileRepository::default());
            let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
            let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
            let genres = Arc::new(InMemoryGenreRepository::default());
            Self {
                repo: InMemoryCatalogRepository::new(movies.clone(), shows.clone(), genres.clone()),
                movies,
                shows,
                genres,
                files,
            }
        }
    }

    #[async_trait::async_trait]
    impl CatalogRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn CatalogRepository {
            &self.repo
        }

        fn movies(&self) -> &dyn MovieRepository {
            self.movies.as_ref()
        }

        fn shows(&self) -> &dyn ShowRepository {
            self.shows.as_ref()
        }

        fn genres(&self) -> &dyn GenreRepository {
            self.genres.as_ref()
        }

        fn files(&self) -> &dyn FileRepository {
            self.files.as_ref()
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

    crate::catalog_repository_contract!(setup);
}
