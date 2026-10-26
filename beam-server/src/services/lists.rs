//! The watchlist and favourites (issue #262): adding a movie or a show to
//! one, taking it off, paging through one, and the flags a title's detail
//! carries in its `user_state`. The user is always the session's, never the
//! request's.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::access::AccessPolicy;
use beam_domain::models::user_list::{UserList, UserListEntry, UserListPosition};
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::{MovieRepository, ShowRepository, UserListRepository};

use crate::models::search::PageInfo;
use crate::models::{MediaMetadata, UserListConnection, UserListItem};
use crate::services::metadata::MetadataService;
use crate::services::playback::PlaybackService;
use crate::services::user_list_cursor;

/// The page size when a request names none.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// The largest page a request may ask for.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Why a list request was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// No movie, show, season or episode has the id.
    #[error("media not found: {0}")]
    MediaNotFound(String),
    /// The id names a season or an episode: only a movie or a show is put on
    /// a list.
    #[error("not a title: {0}")]
    NotATitle(String),
    /// `after` is not a cursor this list issued.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// `first` is outside `1..=MAX_PAGE_SIZE`.
    #[error("invalid pagination: {0}")]
    InvalidPagination(String),
    /// A store failed.
    #[error("internal list error: {0}")]
    Internal(String),
}

fn internal(err: impl std::fmt::Display) -> ListError {
    ListError::Internal(err.to_string())
}

impl From<DbErr> for ListError {
    fn from(err: DbErr) -> Self {
        internal(err)
    }
}

/// The key a title's browse result is found by among a page of them.
fn key_of(media: &MediaMetadata) -> TitleRef {
    match media {
        MediaMetadata::Movie(movie) => TitleRef::Movie(movie.id),
        MediaMetadata::Show(show) => TitleRef::Show(show.id),
    }
}

/// Reads and writes users' watchlists and favourites.
#[derive(Debug)]
pub struct ListService {
    lists: Arc<dyn UserListRepository>,
    movies: Arc<dyn MovieRepository>,
    shows: Arc<dyn ShowRepository>,
    metadata: Arc<dyn MetadataService>,
    playback: Arc<dyn PlaybackService>,
}

/// Every store and service [`ListService`] reads.
#[derive(Debug)]
pub struct ListDependencies {
    pub lists: Arc<dyn UserListRepository>,
    /// Resolve an id to the movie or show it names.
    pub movies: Arc<dyn MovieRepository>,
    pub shows: Arc<dyn ShowRepository>,
    /// A page's titles, as browse lists them.
    pub metadata: Arc<dyn MetadataService>,
    /// The viewer's state for each movie on a page.
    pub playback: Arc<dyn PlaybackService>,
}

impl ListService {
    pub fn new(dependencies: ListDependencies) -> Self {
        let ListDependencies {
            lists,
            movies,
            shows,
            metadata,
            playback,
        } = dependencies;
        Self {
            lists,
            movies,
            shows,
            metadata,
            playback,
        }
    }

    /// The movie or show `id` names. A season's or an episode's id is
    /// [`ListError::NotATitle`]; an id naming nothing, [`ListError::MediaNotFound`].
    async fn title(&self, id: Uuid) -> Result<TitleRef, ListError> {
        if self.movies.find_by_id(id).await?.is_some() {
            return Ok(TitleRef::Movie(id));
        }
        if self.shows.find_by_id(id).await?.is_some() {
            return Ok(TitleRef::Show(id));
        }
        if self.shows.find_season_by_id(id).await?.is_some() {
            return Err(ListError::NotATitle(format!(
                "{id} is a season; put its show on the list"
            )));
        }
        if self.shows.find_episode_by_id(id).await?.is_some() {
            return Err(ListError::NotATitle(format!(
                "{id} is an episode; put its show on the list"
            )));
        }
        Err(ListError::MediaNotFound(format!("media {id} not found")))
    }

    /// Put the movie or show `id` names on the user's `list`. One already
    /// there keeps its place.
    pub async fn add(&self, user_id: Uuid, list: UserList, id: Uuid) -> Result<(), ListError> {
        let title = self.title(id).await?;
        self.lists.add(user_id, list, title).await?;
        Ok(())
    }

    /// Take the movie or show `id` names off the user's `list`; one not on
    /// it changes nothing.
    pub async fn remove(&self, user_id: Uuid, list: UserList, id: Uuid) -> Result<(), ListError> {
        let title = self.title(id).await?;
        self.lists.remove(user_id, list, title).await?;
        Ok(())
    }

    /// One page of the user's `list`, newest addition first, each title as
    /// browse lists it with the user's state for a movie. Only the titles
    /// `access` admits are listed or counted (issue #328): one it hides
    /// keeps its entry, listed again once the policy admits it.
    pub async fn page(
        &self,
        user_id: Uuid,
        access: &AccessPolicy,
        list: UserList,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<UserListConnection, ListError> {
        let size = first.unwrap_or(DEFAULT_PAGE_SIZE);
        let size = NonZeroU32::new(size)
            .filter(|size| size.get() <= MAX_PAGE_SIZE)
            .ok_or_else(|| {
                ListError::InvalidPagination(format!(
                    "a page holds 1 to {MAX_PAGE_SIZE} titles, not {size}"
                ))
            })?;
        let after = after
            .as_deref()
            .map(|cursor| user_list_cursor::decode(list, cursor))
            .transpose()
            .map_err(|err| ListError::InvalidCursor(err.to_string()))?;
        let has_previous_page = after.is_some();

        // One entry past the page says whether another follows.
        let size = size.get() as usize;
        let mut entries: Vec<UserListEntry> = self
            .lists
            .find_page(user_id, list, access, after, size as u64 + 1)
            .await?;
        let has_next_page = entries.len() > size;
        entries.truncate(size);
        let total = self.lists.count(user_id, list, access).await?;

        let titles: Vec<TitleRef> = entries.iter().map(|entry| entry.title).collect();
        let mut media = self.metadata.get_titles(&titles).await.map_err(internal)?;
        self.playback
            .overlay_titles(user_id, &mut media)
            .await
            .map_err(internal)?;
        let mut media: HashMap<TitleRef, MediaMetadata> =
            media.into_iter().map(|m| (key_of(&m), m)).collect();
        // A title removed between the two reads takes its entry with it.
        let items = entries
            .iter()
            .filter_map(|entry| {
                media.remove(&entry.title).map(|media| UserListItem {
                    added_at: entry.added_at,
                    media,
                })
            })
            .collect();

        let cursor_of =
            |entry: &UserListEntry| user_list_cursor::encode(list, &UserListPosition::from(entry));
        Ok(UserListConnection {
            items,
            page_info: PageInfo {
                has_next_page,
                has_previous_page,
                start_cursor: entries.first().map(cursor_of),
                end_cursor: entries.last().map(cursor_of),
            },
            total,
        })
    }

    /// Set the `in_watchlist` and `in_favourites` flags of a title's detail,
    /// read after its watched state, which this leaves as it is.
    pub async fn overlay_media(
        &self,
        user_id: Uuid,
        media: &mut MediaMetadata,
    ) -> Result<(), ListError> {
        let lists = self.lists.lists_of(user_id, key_of(media)).await?;
        let on = |list| Some(lists.contains(&list));
        match media {
            MediaMetadata::Movie(movie) => {
                movie.user_state.in_watchlist = on(UserList::Watchlist);
                movie.user_state.in_favourites = on(UserList::Favourites);
            }
            MediaMetadata::Show(show) => {
                if let Some(state) = show.user_state.as_mut() {
                    state.in_watchlist = on(UserList::Watchlist);
                    state.in_favourites = on(UserList::Favourites);
                }
            }
        }
        Ok(())
    }
}
