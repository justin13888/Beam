use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::models::search::{MediaConnection, MediaSortField, PageInfo, SortOrder, TitleKind};
use crate::models::{
    ArtworkKind, ArtworkVariant, CollectionRef, EpisodeDetail, EpisodeMetadata, MediaMetadata,
    MediaSource, SeasonDetail, UserTitleState, artwork_path,
};
use crate::services::cursor::browse;
use crate::services::paging::{PageDirection, PageRequest};
use crate::services::people::title_credits;
use crate::services::sources::{PrimarySource, SourceCatalog};
use beam_domain::models::access::AccessPolicy;
use beam_domain::models::catalog::{
    self, CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    ShowChildCounts, SortDirection,
};
use beam_domain::models::genre::slugify;
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::{
    CatalogRepository, CollectionRepository, GenreRepository, MovieRepository, PersonRepository,
    ShowRepository,
};
use beam_domain::utils::next_up::{Neighbours, neighbours};

mod mapping;

use mapping::{MovieFiles, movie_metadata, season_metadata, show_metadata, show_ref};

#[async_trait::async_trait]
pub trait MetadataService: Send + Sync + std::fmt::Debug {
    /// A title's full metadata by id: `Ok(None)` when no movie or show has
    /// that id, and an error -- never `None` -- when the lookup itself failed.
    async fn get_media_metadata(
        &self,
        media_id: Uuid,
    ) -> Result<Option<MediaMetadata>, MetadataError>;

    /// One page of browse or search results.
    ///
    /// Fails with [`MetadataError::InvalidPagination`] for a page request the
    /// server does not answer, [`MetadataError::InvalidCursor`] for a cursor
    /// it did not issue for this sort, [`MetadataError::InvalidSearchQuery`]
    /// for a search text no title can contain or a relevance sort without
    /// one, and
    /// [`MetadataError::InternalError`] when the store fails -- never with an
    /// empty page.
    async fn search_media(&self, request: BrowseRequest) -> Result<MediaConnection, MetadataError>;

    /// List the playable/downloadable source files for a playable media id,
    /// the primary first. Movie ids and episode ids are both accepted (a show
    /// id is not -- it has no files of its own; callers use its episode ids
    /// instead). An episode with no files yet resolves to an empty list
    /// rather than an error.
    ///
    /// Fails with [`MetadataError::Unsupported`] when `media_id` is a show's,
    /// [`MetadataError::MediaNotFound`] when no movie, episode or show has
    /// it, and [`MetadataError::InternalError`] when a store fails.
    async fn get_media_sources(&self, media_id: Uuid) -> Result<Vec<MediaSource>, MetadataError>;

    /// One episode by id, with its season, its show and its neighbours:
    /// `Ok(None)` when no episode has that id (issue #188).
    async fn get_episode_detail(
        &self,
        episode_id: Uuid,
    ) -> Result<Option<EpisodeDetail>, MetadataError>;

    /// One season by id, with its episodes and its show: `Ok(None)` when no
    /// season has that id.
    async fn get_season_detail(
        &self,
        season_id: Uuid,
    ) -> Result<Option<SeasonDetail>, MetadataError>;

    /// The titles `titles` names, each as a browse result lists it, in the
    /// order named: a title that no longer exists is skipped (issue #262).
    /// A fixed number of reads, however many titles are named.
    async fn get_titles(&self, titles: &[TitleRef]) -> Result<Vec<MediaMetadata>, MetadataError>;
}

/// Everything one browse or search request asks, in the Relay vocabulary the
/// route accepts.
#[derive(Clone, Debug)]
pub struct BrowseRequest {
    pub first: Option<u32>,
    pub after: Option<String>,
    pub last: Option<u32>,
    pub before: Option<String>,
    pub sort_by: MediaSortField,
    pub sort_order: SortOrder,
    pub filters: MediaSearchFilters,
    /// The caller's access policy: only the titles it admits are listed
    /// (issue #263).
    pub access: AccessPolicy,
}

/// The page size when a request names none.
pub const DEFAULT_PAGE_SIZE: u32 = 20;
/// The largest page a request may ask for: one page's hydration stays bounded.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Database-backed metadata service
#[derive(Debug)]
pub struct DbMetadataService {
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
    sources: Arc<SourceCatalog>,
    catalog_repo: Arc<dyn CatalogRepository>,
    genre_repo: Arc<dyn GenreRepository>,
    person_repo: Arc<dyn PersonRepository>,
    collection_repo: Arc<dyn CollectionRepository>,
}

/// Every store [`DbMetadataService`] reads.
#[derive(Debug)]
pub struct MetadataRepositories {
    pub movies: Arc<dyn MovieRepository>,
    pub shows: Arc<dyn ShowRepository>,
    /// A title's files, ranked, with their tracks.
    pub sources: Arc<SourceCatalog>,
    pub catalog: Arc<dyn CatalogRepository>,
    pub genres: Arc<dyn GenreRepository>,
    /// A title's cast and crew, which its detail lists (issue #261).
    pub people: Arc<dyn PersonRepository>,
    /// The collection a movie's detail names (issue #262).
    pub collections: Arc<dyn CollectionRepository>,
}

fn internal(err: impl std::fmt::Display) -> MetadataError {
    MetadataError::InternalError(err.to_string())
}

impl DbMetadataService {
    pub fn new(repositories: MetadataRepositories) -> Self {
        let MetadataRepositories {
            movies,
            shows,
            sources,
            catalog,
            genres,
            people,
            collections,
        } = repositories;
        Self {
            movie_repo: movies,
            show_repo: shows,
            sources,
            catalog_repo: catalog,
            genre_repo: genres,
            person_repo: people,
            collection_repo: collections,
        }
    }

    /// Build MediaMetadata for a movie by its DB model
    async fn build_movie_metadata(
        &self,
        movie: beam_domain::models::Movie,
    ) -> Result<MediaMetadata, MetadataError> {
        let files = MovieFiles::from(
            self.sources
                .movie_primary(movie.id)
                .await
                .map_err(internal)?,
        );

        let genres = self
            .genre_repo
            .movie_genre_names(&[movie.id])
            .await
            .map_err(internal)?
            .remove(&movie.id)
            .unwrap_or_default();
        let credits = title_credits(
            self.person_repo
                .movie_credits(movie.id)
                .await
                .map_err(internal)?,
        );
        let collection = self
            .collection_repo
            .movie_collection(movie.id)
            .await
            .map_err(internal)?
            .map(|collection| CollectionRef {
                id: collection.id,
                name: collection.name,
            });
        Ok(movie_metadata(
            movie,
            genres,
            files,
            Some(credits),
            collection,
        ))
    }

    /// Build MediaMetadata for a show by its DB model
    async fn build_show_metadata(
        &self,
        show: beam_domain::models::Show,
    ) -> Result<MediaMetadata, MetadataError> {
        let seasons_domain = self
            .show_repo
            .find_seasons_by_show_id(show.id)
            .await
            .map_err(internal)?;

        let mut seasons = Vec::new();
        let mut episode_count = 0_u32;
        for season in seasons_domain {
            let episodes_domain = self
                .show_repo
                .find_episodes_by_season_id(season.id)
                .await
                .map_err(internal)?;

            let mut episodes = Vec::new();
            for ep in episodes_domain {
                episodes.push(self.episode_metadata(ep).await?);
            }
            episode_count += episodes.len() as u32;
            seasons.push(season_metadata(season, episodes));
        }

        let genres = self
            .genre_repo
            .show_genre_names(&[show.id])
            .await
            .map_err(internal)?
            .remove(&show.id)
            .unwrap_or_default();
        let counts = ShowChildCounts {
            seasons: seasons.len() as u32,
            episodes: episode_count,
        };
        let credits = title_credits(
            self.person_repo
                .show_credits(show.id)
                .await
                .map_err(internal)?,
        );
        Ok(show_metadata(show, genres, counts, seasons, Some(credits)))
    }

    /// One episode's metadata, its primary source read from its files.
    async fn episode_metadata(
        &self,
        ep: beam_domain::models::Episode,
    ) -> Result<EpisodeMetadata, MetadataError> {
        let PrimarySource {
            file_id,
            duration_secs,
            last_episode_number,
            source_count,
        } = self.sources.episode_primary(&ep).await.map_err(internal)?;
        // A file holding a run of episodes lasts the whole run, which is not
        // this episode's duration.
        let duration = duration_secs.filter(|_| last_episode_number.is_none());
        let beam_domain::models::Episode {
            id,
            season_id: _,
            episode_number,
            title,
            description,
            air_date,
            runtime: _,
            thumbnail_url,
            created_at: _,
        } = ep;
        Ok(EpisodeMetadata {
            id,
            episode_number,
            title,
            description,
            air_date,
            thumbnail_url: thumbnail_url
                .as_ref()
                .map(|_| artwork_path(ArtworkKind::Episode, id, ArtworkVariant::Thumbnail)),
            duration_secs: duration,
            file_id,
            source_count,
            user_state: UserTitleState::default(),
        })
    }

    /// The titles behind one page of catalogue positions, in page order. A
    /// title that vanished between the page read and this one is skipped.
    ///
    /// A fixed number of reads per page, however long it is: the titles of
    /// each kind, their genres, and the shows' season and episode counts.
    async fn hydrate(&self, page: &[TitleRef]) -> Result<Vec<MediaMetadata>, MetadataError> {
        let movie_ids: Vec<Uuid> = page
            .iter()
            .filter_map(|title| match title {
                TitleRef::Movie(id) => Some(*id),
                TitleRef::Show(_) => None,
            })
            .collect();
        let show_ids: Vec<Uuid> = page
            .iter()
            .filter_map(|title| match title {
                TitleRef::Show(id) => Some(*id),
                TitleRef::Movie(_) => None,
            })
            .collect();

        let mut movies: HashMap<Uuid, beam_domain::models::Movie> = self
            .movie_repo
            .find_by_ids(&movie_ids)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
        let mut shows: HashMap<Uuid, beam_domain::models::Show> = self
            .show_repo
            .find_by_ids(&show_ids)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
        let mut movie_genres = self
            .genre_repo
            .movie_genre_names(&movie_ids)
            .await
            .map_err(internal)?;
        let mut show_genres = self
            .genre_repo
            .show_genre_names(&show_ids)
            .await
            .map_err(internal)?;
        let counts = self
            .show_repo
            .child_counts(&show_ids)
            .await
            .map_err(internal)?;

        Ok(page
            .iter()
            .filter_map(|title| match *title {
                TitleRef::Movie(id) => movies.remove(&id).map(|movie| {
                    let genres = movie_genres.remove(&movie.id).unwrap_or_default();
                    movie_metadata(movie, genres, MovieFiles::default(), None, None)
                }),
                TitleRef::Show(id) => shows.remove(&id).map(|show| {
                    let genres = show_genres.remove(&show.id).unwrap_or_default();
                    let counts = counts.get(&show.id).copied().unwrap_or_default();
                    show_metadata(show, genres, counts, vec![], None)
                }),
            })
            .collect())
    }
}

impl From<MediaSortField> for CatalogSortField {
    fn from(field: MediaSortField) -> Self {
        match field {
            MediaSortField::Title => Self::Title,
            MediaSortField::Year => Self::Year,
            MediaSortField::Rating => Self::Rating,
            MediaSortField::DateAdded => Self::DateAdded,
            MediaSortField::Runtime => Self::Runtime,
            MediaSortField::Relevance => Self::Relevance,
        }
    }
}

impl From<SortOrder> for SortDirection {
    fn from(order: SortOrder) -> Self {
        match order {
            SortOrder::Asc => Self::Asc,
            SortOrder::Desc => Self::Desc,
        }
    }
}

impl From<TitleKind> for catalog::TitleKind {
    fn from(kind: TitleKind) -> Self {
        match kind {
            TitleKind::Movie => Self::Movie,
            TitleKind::Show => Self::Show,
        }
    }
}

/// A year filter as the catalogue's signed `year` column holds it, or
/// [`MetadataError::InvalidFilter`] naming the parameter when it cannot.
///
/// Checked rather than cast: `4294967295 as i32` is `-1`, which as a
/// `year_from` would have matched every title.
fn year_filter(name: &str, year: Option<u32>) -> Result<Option<i32>, MetadataError> {
    year.map(|year| {
        i32::try_from(year).map_err(|_| {
            MetadataError::InvalidFilter(format!("{name} {year} is not a year a title can have"))
        })
    })
    .transpose()
}

/// The largest `min_rating`: ratings are on the provider's 0-10 scale.
pub const MAX_RATING: f64 = 10.0;

/// A minimum rating in the single precision ratings are stored in, or
/// [`MetadataError::InvalidFilter`] when it is off the 0-10 scale -- `NaN`
/// and the infinities included, which a query string can spell.
fn rating_filter(min_rating: Option<f64>) -> Result<Option<f32>, MetadataError> {
    min_rating
        .map(|rating| {
            if (0.0..=MAX_RATING).contains(&rating) {
                // In range, so the narrowing loses only precision the stored
                // rating does not have.
                Ok(rating as f32)
            } else {
                Err(MetadataError::InvalidFilter(format!(
                    "min_rating {rating} is not on the 0-{MAX_RATING} scale"
                )))
            }
        })
        .transpose()
}

impl TryFrom<MediaSearchFilters> for CatalogFilters {
    type Error = MetadataError;

    fn try_from(filters: MediaSearchFilters) -> Result<Self, MetadataError> {
        let MediaSearchFilters {
            media_type,
            genre,
            year,
            year_from,
            year_to,
            query,
            min_rating,
        } = filters;
        Ok(Self {
            kind: media_type.map(catalog::TitleKind::from),
            query,
            // A genre is matched by slug, so `Science Fiction`,
            // `science fiction` and `science-fiction` all name one genre.
            genre_slug: genre.as_deref().map(slugify),
            year: year_filter("year", year)?,
            year_from: year_filter("year_from", year_from)?,
            year_to: year_filter("year_to", year_to)?,
            min_rating: rating_filter(min_rating)?,
        })
    }
}

#[async_trait::async_trait]
impl MetadataService for DbMetadataService {
    async fn get_media_metadata(&self, id: Uuid) -> Result<Option<MediaMetadata>, MetadataError> {
        if let Some(movie) = self.movie_repo.find_by_id(id).await.map_err(internal)? {
            return self.build_movie_metadata(movie).await.map(Some);
        }
        if let Some(show) = self.show_repo.find_by_id(id).await.map_err(internal)? {
            return self.build_show_metadata(show).await.map(Some);
        }
        Ok(None)
    }

    async fn search_media(&self, request: BrowseRequest) -> Result<MediaConnection, MetadataError> {
        let BrowseRequest {
            first,
            after,
            last,
            before,
            sort_by,
            sort_order,
            filters,
            access,
        } = request;
        let PageRequest {
            direction,
            size,
            cursor,
        } = PageRequest::from_relay(
            first,
            after,
            last,
            before,
            DEFAULT_PAGE_SIZE,
            MAX_PAGE_SIZE,
            "items",
        )
        .map_err(MetadataError::InvalidPagination)?;
        let position = cursor
            .as_deref()
            .map(|cursor| browse::decode(cursor, sort_by, sort_order))
            .transpose()
            .map_err(|e| MetadataError::InvalidCursor(e.to_string()))?;
        let has_cursor = position.is_some();
        // Postgres text cannot hold a NUL, so no title contains one and the
        // store cannot even bind it: it fails the statement (22021), which
        // would answer caller input with a 500. `genre` needs no such check:
        // it is slugified, which drops every NUL.
        if filters
            .query
            .as_deref()
            .is_some_and(|query| query.contains('\0'))
        {
            return Err(MetadataError::InvalidSearchQuery(
                "query must not contain a NUL character".to_string(),
            ));
        }
        // Relevance is closeness to a query; without one there is nothing to
        // rank by, and the store would refuse the listing as a 500.
        if sort_by == MediaSortField::Relevance && filters.query.is_none() {
            return Err(MetadataError::InvalidSearchQuery(
                "sort_by=relevance ranks titles by how closely they match `query`; pass one"
                    .to_string(),
            ));
        }

        // One row past the page says whether another page lies that way.
        let size_usize = size.get() as usize;
        let query = CatalogQuery {
            filters: CatalogFilters::try_from(filters)?,
            access,
            sort: CatalogSort {
                field: sort_by.into(),
                direction: sort_order.into(),
            },
            seek: match direction {
                PageDirection::Forward => Seek::Forward(position),
                PageDirection::Backward => Seek::Backward(position),
            },
            limit: size.saturating_add(1),
        };
        let mut page = self.catalog_repo.browse(&query).await.map_err(internal)?;
        let more = page.len() > size_usize;
        if more {
            match direction {
                // Forwards the extra row is the last; backwards, in display
                // order, it is the first.
                PageDirection::Forward => page.truncate(size_usize),
                PageDirection::Backward => {
                    page.drain(..page.len() - size_usize);
                }
            }
        }
        let (has_next_page, has_previous_page) = match direction {
            PageDirection::Forward => (more, has_cursor),
            PageDirection::Backward => (has_cursor, more),
        };

        let titles: Vec<TitleRef> = page
            .iter()
            .map(|position| match position.kind {
                catalog::TitleKind::Movie => TitleRef::Movie(position.id),
                catalog::TitleKind::Show => TitleRef::Show(position.id),
            })
            .collect();
        let items = self.hydrate(&titles).await?;
        let cursor_of = |position: &CatalogPosition| browse::encode(sort_by, sort_order, position);
        Ok(MediaConnection {
            items,
            page_info: PageInfo {
                has_next_page,
                has_previous_page,
                start_cursor: page.first().map(cursor_of),
                end_cursor: page.last().map(cursor_of),
            },
        })
    }

    async fn get_media_sources(&self, id: Uuid) -> Result<Vec<MediaSource>, MetadataError> {
        if self
            .movie_repo
            .find_by_id(id)
            .await
            .map_err(internal)?
            .is_some()
        {
            return self.sources.movie_sources(id).await.map_err(internal);
        }

        // Episodes are playable ids too: an episode id resolves to its own
        // files, giving series content the same multi-rendition selection as
        // movies. An episode with no files yet is a well-formed empty list
        // (the client treats no sources as "unplayable"), which
        // is distinct from a genuinely unknown id (`MediaNotFound` below).
        if let Some(episode) = self
            .show_repo
            .find_episode_by_id(id)
            .await
            .map_err(internal)?
        {
            return self
                .sources
                .episode_sources(&episode)
                .await
                .map_err(internal);
        }

        if self
            .show_repo
            .find_by_id(id)
            .await
            .map_err(internal)?
            .is_some()
        {
            return Err(MetadataError::Unsupported(
                "sources are not available at the show level; use an episode id".to_string(),
            ));
        }

        Err(MetadataError::MediaNotFound)
    }

    async fn get_episode_detail(
        &self,
        episode_id: Uuid,
    ) -> Result<Option<EpisodeDetail>, MetadataError> {
        let Some(episode) = self
            .show_repo
            .find_episode_by_id(episode_id)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        let Some(season) = self
            .show_repo
            .find_season_by_id(episode.season_id)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        let Some(show) = self
            .show_repo
            .find_by_id(season.show_id)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        // Neighbours by next-up's rules: playable episodes only, and the
        // next after the run the episode's primary file holds.
        let outline = self
            .show_repo
            .episode_outline(show.id)
            .await
            .map_err(internal)?;
        let run_end = self
            .sources
            .episode_primary(&episode)
            .await
            .map_err(internal)?
            .last_episode_number;
        let Neighbours {
            previous: previous_episode_id,
            next: next_episode_id,
        } = neighbours(&outline, episode_id, run_end);
        Ok(Some(EpisodeDetail {
            episode: self.episode_metadata(episode).await?,
            season_id: season.id,
            season_number: season.season_number,
            show: show_ref(&show),
            previous_episode_id,
            next_episode_id,
        }))
    }

    async fn get_season_detail(
        &self,
        season_id: Uuid,
    ) -> Result<Option<SeasonDetail>, MetadataError> {
        let Some(season) = self
            .show_repo
            .find_season_by_id(season_id)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        let Some(show) = self
            .show_repo
            .find_by_id(season.show_id)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        let mut episodes = Vec::new();
        for ep in self
            .show_repo
            .find_episodes_by_season_id(season.id)
            .await
            .map_err(internal)?
        {
            episodes.push(self.episode_metadata(ep).await?);
        }
        Ok(Some(SeasonDetail {
            season: season_metadata(season, episodes),
            show: show_ref(&show),
        }))
    }

    async fn get_titles(&self, titles: &[TitleRef]) -> Result<Vec<MediaMetadata>, MetadataError> {
        self.hydrate(titles).await
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("Media not found")]
    MediaNotFound,
    #[error("Internal metadata service error: {0}")]
    InternalError(String),
    /// A browse cursor this server did not issue for the requested sort.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// A page request the server does not answer: mixed directions, or a size
    /// outside `1..=MAX_PAGE_SIZE`.
    #[error("invalid pagination: {0}")]
    InvalidPagination(String),
    /// A search text no title can contain -- one holding a NUL character --
    /// or a relevance sort with no search text to rank by.
    #[error("invalid search query: {0}")]
    InvalidSearchQuery(String),
    /// A numeric filter outside the range it can take: a year the catalogue's
    /// signed column cannot hold, or a minimum rating off the 0-10 scale.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
    /// The request was well-formed and the target exists, but this operation
    /// doesn't apply to it (e.g. requesting sources for a show id).
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

/// Search filters for media
#[derive(Clone, Debug, Default)]
pub struct MediaSearchFilters {
    pub media_type: Option<TitleKind>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
    pub query: Option<String>,
    /// Minimum rating on the provider's 0-10 scale.
    pub min_rating: Option<f64>,
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod metadata_tests;
