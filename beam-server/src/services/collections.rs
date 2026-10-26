//! Provider collections (issue #262): the listing of the collections the
//! library's movies are in, and one collection's movies.

use std::cmp::Ordering;
use std::num::NonZeroU32;
use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::collection::{Collection as StoredCollection, CollectionSummary};
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::CollectionRepository;

use crate::models::{
    ArtworkKind, ArtworkVariant, Collection, CollectionConnection, CollectionDetail, MediaMetadata,
    MovieMetadata, artwork_path,
};
use crate::services::metadata::MetadataService;
use crate::services::offset_cursor::{self, OffsetList};
use crate::services::playback::PlaybackService;

/// The page size when a request names none.
pub const DEFAULT_PAGE_SIZE: u32 = 20;
/// The largest page a request may ask for.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Why a collection request was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    /// `after` is not a cursor the listing issued.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// `first` is outside `1..=MAX_PAGE_SIZE`.
    #[error("invalid pagination: {0}")]
    InvalidPagination(String),
    /// A store failed.
    #[error("internal collection error: {0}")]
    Internal(String),
}

fn internal(err: impl std::fmt::Display) -> CollectionError {
    CollectionError::Internal(err.to_string())
}

/// A collection's art, as the paths Beam serves it from.
fn art(collection: &StoredCollection) -> (Option<String>, Option<String>) {
    let path = |url: &Option<String>, variant| {
        url.as_ref()
            .map(|_| artwork_path(ArtworkKind::Collection, collection.id, variant))
    };
    (
        path(&collection.poster_url, ArtworkVariant::Poster),
        path(&collection.backdrop_url, ArtworkVariant::Backdrop),
    )
}

/// The order a collection lists its movies in: by the year each was
/// released -- its release date's, else its own `year` -- and within a year
/// by release date, a movie with no year (or no date) after those with one;
/// then by title ignoring case, then by id, so the order is total.
fn release_order(a: &MovieMetadata, b: &MovieMetadata) -> Ordering {
    fn known<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let year = |movie: &MovieMetadata| {
        movie
            .release_date
            .map(|date| chrono::Datelike::year(&date))
            .or_else(|| movie.year.and_then(|year| i32::try_from(year).ok()))
    };
    known(year(a), year(b))
        .then_with(|| known(a.release_date, b.release_date))
        .then_with(|| {
            a.title
                .original
                .to_lowercase()
                .cmp(&b.title.original.to_lowercase())
        })
        .then_with(|| a.id.cmp(&b.id))
}

/// Reads provider collections.
#[derive(Debug)]
pub struct CollectionService {
    collections: Arc<dyn CollectionRepository>,
    metadata: Arc<dyn MetadataService>,
    playback: Arc<dyn PlaybackService>,
}

impl CollectionService {
    pub fn new(
        collections: Arc<dyn CollectionRepository>,
        metadata: Arc<dyn MetadataService>,
        playback: Arc<dyn PlaybackService>,
    ) -> Self {
        Self {
            collections,
            metadata,
            playback,
        }
    }

    /// The stored collection `id` names, art URLs and all: what the artwork
    /// route fetches a collection's art by. `Ok(None)` for one no movie is
    /// in any more, which the listing no longer lists either.
    pub async fn find_collection(&self, id: Uuid) -> Result<Option<StoredCollection>, DbErr> {
        let Some(collection) = self.collections.find_by_id(id).await? else {
            return Ok(None);
        };
        if self.collections.movie_ids(id).await?.is_empty() {
            return Ok(None);
        }
        Ok(Some(collection))
    }

    /// One page of the collections the library's movies are in, by name.
    pub async fn list(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<CollectionConnection, CollectionError> {
        let size = first.unwrap_or(DEFAULT_PAGE_SIZE);
        let size = NonZeroU32::new(size)
            .filter(|size| size.get() <= MAX_PAGE_SIZE)
            .ok_or_else(|| {
                CollectionError::InvalidPagination(format!(
                    "a page holds 1 to {MAX_PAGE_SIZE} collections, not {size}"
                ))
            })?;
        let offset = offset_cursor::start_after(OffsetList::Collections, after.as_deref())
            .map_err(|err| CollectionError::InvalidCursor(err.to_string()))?;

        let page = self
            .collections
            .list(u64::from(size.get()), offset)
            .await
            .map_err(internal)?;
        let total = self.collections.count().await.map_err(internal)?;
        Ok(CollectionConnection {
            page_info: offset_cursor::page_info(OffsetList::Collections, offset, page.len(), total),
            items: page
                .into_iter()
                .map(|summary| {
                    let CollectionSummary {
                        collection,
                        movie_count,
                    } = summary;
                    let (poster_url, backdrop_url) = art(&collection);
                    Collection {
                        id: collection.id,
                        name: collection.name,
                        poster_url,
                        backdrop_url,
                        movie_count,
                    }
                })
                .collect(),
            total,
        })
    }

    /// One collection with its movies, in release order, each with the
    /// viewer's state: `Ok(None)` when no collection has the id, or no movie
    /// is in it any more.
    pub async fn detail(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<Option<CollectionDetail>, CollectionError> {
        let Some(collection) = self.collections.find_by_id(id).await.map_err(internal)? else {
            return Ok(None);
        };
        let movie_ids = self.collections.movie_ids(id).await.map_err(internal)?;
        if movie_ids.is_empty() {
            return Ok(None);
        }
        let titles: Vec<TitleRef> = movie_ids.into_iter().map(TitleRef::Movie).collect();
        let mut media = self.metadata.get_titles(&titles).await.map_err(internal)?;
        self.playback
            .overlay_titles(user_id, &mut media)
            .await
            .map_err(internal)?;
        let mut movies: Vec<MovieMetadata> = media
            .into_iter()
            .filter_map(|media| match media {
                MediaMetadata::Movie(movie) => Some(movie),
                MediaMetadata::Show(_) => None,
            })
            .collect();
        movies.sort_by(release_order);

        let (poster_url, backdrop_url) = art(&collection);
        Ok(Some(CollectionDetail {
            id: collection.id,
            name: collection.name,
            poster_url,
            backdrop_url,
            movies,
        }))
    }
}
