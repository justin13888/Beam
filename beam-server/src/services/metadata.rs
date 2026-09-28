use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::models::{
    ArtworkKind, ArtworkVariant, EpisodeDetail, EpisodeMetadata, ExternalIdentifiers,
    MediaMetadata, MediaSource, MovieMetadata, Ratings, SeasonDetail, SeasonMetadata, ShowDates,
    ShowMetadata, ShowRef, Title, UserTitleState, artwork_path,
};
use crate::services::cursor;
use crate::services::sources::{PrimarySource, SourceCatalog};
use beam_domain::models::catalog::{
    CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    ShowChildCounts, SortDirection, TitleKind,
};
use beam_domain::repositories::genre::slugify;
use beam_domain::repositories::{
    CatalogRepository, GenreRepository, MovieRepository, ShowRepository,
};

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
    /// for a search text no title can contain, and
    /// [`MetadataError::InternalError`] when the store fails -- never with an
    /// empty page.
    async fn search_media(&self, request: BrowseRequest) -> Result<MediaConnection, MetadataError>;

    /// List the playable/downloadable source files for a playable media id,
    /// the primary first. Movie ids and episode ids are both accepted (a show
    /// id is not -- it has no files of its own; callers use its episode ids
    /// instead). An episode with no files yet resolves to an empty list
    /// rather than an error.
    ///
    /// A `media_id` that is not a UUID is [`MetadataError::InvalidId`], kept
    /// distinct from `MediaNotFound` because the routes answer them 400 and
    /// 404 respectively.
    async fn get_media_sources(&self, media_id: &str) -> Result<Vec<MediaSource>, MetadataError>;

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
}

/// The page size when a request names none.
pub const DEFAULT_PAGE_SIZE: u32 = 20;
/// The largest page a request may ask for: one page's hydration stays bounded.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Which way a page is read from its cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageDirection {
    /// `first`, optionally `after` a cursor.
    Forward,
    /// `last`, optionally `before` a cursor.
    Backward,
}

/// A validated page request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub direction: PageDirection,
    pub size: NonZeroU32,
    pub cursor: Option<String>,
}

impl PageRequest {
    /// Read the four Relay parameters into one page request.
    ///
    /// Accepted are `first`/`after` (forwards, the default) and
    /// `last`/`before` (backwards), each with or without its cursor. Mixing
    /// the two directions is refused rather than guessed at, and so is a size
    /// outside `1..=MAX_PAGE_SIZE`.
    pub fn from_relay(
        first: Option<u32>,
        after: Option<String>,
        last: Option<u32>,
        before: Option<String>,
    ) -> Result<Self, MetadataError> {
        let invalid = |detail: &str| Err(MetadataError::InvalidPagination(detail.to_string()));
        let forward = first.is_some() || after.is_some();
        let backward = last.is_some() || before.is_some();
        if forward && backward {
            return invalid(
                "`first`/`after` page forwards and `last`/`before` page backwards; send one pair",
            );
        }
        let (direction, size, cursor) = if backward {
            (PageDirection::Backward, last, before)
        } else {
            (PageDirection::Forward, first, after)
        };
        let size = size.unwrap_or(DEFAULT_PAGE_SIZE);
        let Some(size) = NonZeroU32::new(size).filter(|size| size.get() <= MAX_PAGE_SIZE) else {
            return invalid(&format!(
                "a page holds 1 to {MAX_PAGE_SIZE} items, not {size}"
            ));
        };
        Ok(Self {
            direction,
            size,
            cursor,
        })
    }
}

/// Database-backed metadata service
#[derive(Debug)]
pub struct DbMetadataService {
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
    sources: Arc<SourceCatalog>,
    catalog_repo: Arc<dyn CatalogRepository>,
    genre_repo: Arc<dyn GenreRepository>,
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
}

fn internal(err: impl std::fmt::Display) -> MetadataError {
    MetadataError::InternalError(err.to_string())
}

/// A provider rating on its 0-10 scale as the wire's percentage.
fn ratings(rating_tmdb: Option<f32>) -> Option<Ratings> {
    rating_tmdb.map(|r| Ratings {
        tmdb: Some((r * 10.0) as u32),
    })
}

fn identifiers(
    imdb_id: Option<String>,
    tmdb_id: Option<u32>,
    tvdb_id: Option<u32>,
) -> Option<ExternalIdentifiers> {
    (imdb_id.is_some() || tmdb_id.is_some() || tvdb_id.is_some()).then_some(ExternalIdentifiers {
        imdb_id,
        tmdb_id,
        tvdb_id,
    })
}

fn midnight(date: chrono::NaiveDate) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0).unwrap_or_default(),
        chrono::Utc,
    )
}

/// The parts of a movie's metadata that come from files, which browse does
/// not read.
#[derive(Debug, Default)]
struct MovieFiles {
    duration: Option<f64>,
    file_id: Option<Uuid>,
    source_count: Option<u32>,
}

impl From<PrimarySource> for MovieFiles {
    fn from(primary: PrimarySource) -> Self {
        let PrimarySource {
            file_id,
            duration_secs,
            spans_episodes: _,
            source_count,
        } = primary;
        Self {
            duration: duration_secs,
            file_id,
            source_count: Some(source_count),
        }
    }
}

fn movie_metadata(
    movie: beam_domain::models::Movie,
    genres: Vec<String>,
    files: MovieFiles,
) -> MediaMetadata {
    let beam_domain::models::Movie {
        id,
        title,
        identity_key: _,
        pinned_ref: _,
        pin_source: _,
        title_localized,
        description,
        year,
        release_date,
        runtime,
        poster_url,
        backdrop_url,
        tmdb_id,
        imdb_id,
        tvdb_id,
        anilist_id: _,
        rating_tmdb,
        rating_imdb: _,
        created_at: _,
        updated_at: _,
    } = movie;
    let MovieFiles {
        duration,
        file_id,
        source_count,
    } = files;
    MediaMetadata::Movie(MovieMetadata {
        id,
        title: Title {
            original: title,
            localized: title_localized,
            alternatives: None,
        },
        description,
        year,
        release_date: release_date.map(midnight),
        runtime: runtime.map(|d| (d.as_secs() / 60) as u32),
        duration,
        poster_url: poster_url
            .map(|_| artwork_path(ArtworkKind::Movie, id, ArtworkVariant::Poster)),
        backdrop_url: backdrop_url
            .map(|_| artwork_path(ArtworkKind::Movie, id, ArtworkVariant::Backdrop)),
        genres,
        ratings: ratings(rating_tmdb),
        identifiers: identifiers(imdb_id, tmdb_id, tvdb_id),
        file_id,
        source_count,
        user_state: UserTitleState::default(),
    })
}

fn show_metadata(
    show: beam_domain::models::Show,
    genres: Vec<String>,
    counts: ShowChildCounts,
    seasons: Vec<SeasonMetadata>,
) -> MediaMetadata {
    let beam_domain::models::Show {
        id,
        title,
        identity_key: _,
        pinned_ref: _,
        pin_source: _,
        title_localized,
        description,
        year,
        poster_url,
        backdrop_url,
        tmdb_id,
        imdb_id,
        tvdb_id,
        anilist_id: _,
        rating_tmdb,
        created_at: _,
        updated_at: _,
    } = show;
    let ShowChildCounts {
        seasons: season_count,
        episodes: episode_count,
    } = counts;
    MediaMetadata::Show(ShowMetadata {
        id,
        title: Title {
            original: title,
            localized: title_localized,
            alternatives: None,
        },
        description,
        year,
        poster_url: poster_url.map(|_| artwork_path(ArtworkKind::Show, id, ArtworkVariant::Poster)),
        backdrop_url: backdrop_url
            .map(|_| artwork_path(ArtworkKind::Show, id, ArtworkVariant::Backdrop)),
        genres,
        ratings: ratings(rating_tmdb),
        identifiers: identifiers(imdb_id, tmdb_id, tvdb_id),
        season_count,
        episode_count,
        seasons,
        user_state: None,
    })
}

/// The show heading an episode's or a season's detail.
fn show_ref(show: &beam_domain::models::Show) -> ShowRef {
    ShowRef {
        id: show.id,
        title: Title {
            original: show.title.clone(),
            localized: show.title_localized.clone(),
            alternatives: None,
        },
        poster_url: show
            .poster_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Poster)),
        backdrop_url: show
            .backdrop_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Backdrop)),
    }
}

fn season_metadata(
    season: beam_domain::models::Season,
    episodes: Vec<EpisodeMetadata>,
) -> SeasonMetadata {
    let dates = ShowDates {
        first_aired: season.first_aired.map(midnight),
        last_aired: season.last_aired.map(midnight),
    };
    SeasonMetadata {
        id: season.id,
        season_number: season.season_number,
        dates,
        episode_runtime: None,
        episodes,
        poster_url: season
            .poster_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Season, season.id, ArtworkVariant::Poster)),
        genres: vec![],
        ratings: None,
        identifiers: None,
        user_state: None,
    }
}

impl DbMetadataService {
    pub fn new(repositories: MetadataRepositories) -> Self {
        let MetadataRepositories {
            movies,
            shows,
            sources,
            catalog,
            genres,
        } = repositories;
        Self {
            movie_repo: movies,
            show_repo: shows,
            sources,
            catalog_repo: catalog,
            genre_repo: genres,
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
        Ok(movie_metadata(movie, genres, files))
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
        Ok(show_metadata(show, genres, counts, seasons))
    }

    /// One episode's metadata, its primary source read from its files.
    async fn episode_metadata(
        &self,
        ep: beam_domain::models::Episode,
    ) -> Result<EpisodeMetadata, MetadataError> {
        let PrimarySource {
            file_id,
            duration_secs,
            spans_episodes,
            source_count,
        } = self.sources.episode_primary(&ep).await.map_err(internal)?;
        // A file holding a run of episodes lasts the whole run, which is not
        // this episode's duration.
        let duration = duration_secs.filter(|_| !spans_episodes);
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
            duration,
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
    async fn hydrate(&self, page: &[CatalogPosition]) -> Result<Vec<MediaMetadata>, MetadataError> {
        let ids_of = |kind: TitleKind| -> Vec<Uuid> {
            page.iter()
                .filter(|p| p.kind == kind)
                .map(|p| p.id)
                .collect()
        };
        let movie_ids = ids_of(TitleKind::Movie);
        let show_ids = ids_of(TitleKind::Show);

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
            .filter_map(|position| match position.kind {
                TitleKind::Movie => movies.remove(&position.id).map(|movie| {
                    let genres = movie_genres.remove(&movie.id).unwrap_or_default();
                    movie_metadata(movie, genres, MovieFiles::default())
                }),
                TitleKind::Show => shows.remove(&position.id).map(|show| {
                    let genres = show_genres.remove(&show.id).unwrap_or_default();
                    let counts = counts.get(&show.id).copied().unwrap_or_default();
                    show_metadata(show, genres, counts, vec![])
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

impl From<MediaTypeFilter> for TitleKind {
    fn from(kind: MediaTypeFilter) -> Self {
        match kind {
            MediaTypeFilter::Movie => Self::Movie,
            MediaTypeFilter::Show => Self::Show,
        }
    }
}

impl From<MediaSearchFilters> for CatalogFilters {
    fn from(filters: MediaSearchFilters) -> Self {
        let MediaSearchFilters {
            media_type,
            genre,
            year,
            year_from,
            year_to,
            query,
            min_rating,
        } = filters;
        Self {
            kind: media_type.map(TitleKind::from),
            query,
            // A genre is matched by slug, so `Science Fiction`,
            // `science fiction` and `science-fiction` all name one genre.
            genre_slug: genre.as_deref().map(slugify),
            year,
            year_from,
            year_to,
            min_rating,
        }
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
        } = request;
        let PageRequest {
            direction,
            size,
            cursor,
        } = PageRequest::from_relay(first, after, last, before)?;
        let position = cursor
            .as_deref()
            .map(|cursor| cursor::decode(cursor, sort_by, sort_order))
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

        // One row past the page says whether another page lies that way.
        let size_usize = size.get() as usize;
        let query = CatalogQuery {
            filters: filters.into(),
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

        let items = self.hydrate(&page).await?;
        let cursor_of = |position: &CatalogPosition| cursor::encode(sort_by, sort_order, position);
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

    async fn get_media_sources(&self, media_id: &str) -> Result<Vec<MediaSource>, MetadataError> {
        let id = Uuid::parse_str(media_id).map_err(|_| MetadataError::InvalidId)?;

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
        // Neighbours in the order next-up walks: by season, then episode,
        // specials first.
        let outline = self
            .show_repo
            .episode_outline(show.id)
            .await
            .map_err(internal)?;
        let at = outline.iter().position(|e| e.episode_id == episode_id);
        let previous_episode_id = at
            .and_then(|at| at.checked_sub(1))
            .and_then(|before| outline.get(before))
            .map(|e| e.episode_id);
        let next_episode_id = at.and_then(|at| outline.get(at + 1)).map(|e| e.episode_id);
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
}

#[derive(Debug, Error)]
pub enum MetadataError {
    /// The caller's media id is not a UUID.
    ///
    /// Its own variant because it is the caller's mistake, not the server's:
    /// folding it into `InternalError` is what made a malformed id a 500 on
    /// `/v1/media/{id}/sources` and `/v1/admin/media/{id}/refresh` (issue
    /// #123).
    #[error("invalid media id")]
    InvalidId,
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
    /// A search text no title can contain: one holding a NUL character.
    #[error("invalid search query: {0}")]
    InvalidSearchQuery(String),
    /// The request was well-formed and the target exists, but this operation
    /// doesn't apply to it (e.g. requesting sources for a show id).
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

pub use crate::models::search::{
    MediaConnection, MediaSortField, MediaTypeFilter, PageInfo, SortOrder,
};

/// Search filters for media
#[derive(Clone, Debug, Default)]
pub struct MediaSearchFilters {
    pub media_type: Option<MediaTypeFilter>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
    pub query: Option<String>,
    pub min_rating: Option<u32>,
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod metadata_tests;
