//! Browsing: the catalog, a title's detail and watched state, libraries and
//! their files, continue watching and history.

use super::{BeamClient, parse_uuid};
use crate::catalog::{
    BrowseQuery, ContinueWatchingEntry, EpisodeSummary, HistoryPage, LibraryFileSummary,
    LibrarySummary, MediaDetail, MediaPage, browse_params,
};
use crate::error::BeamError;

/// The page size [`BeamClient::library_files`] walks a library's files at:
/// the largest `GET /v1/libraries/{id}/files` takes, so the fewest round
/// trips.
const LIBRARY_FILES_PAGE: i64 = 500;

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// One page of the catalog, filtered and sorted.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] for a query the server would reject,
    /// and propagates transport failures.
    pub async fn browse_media(&self, query: BrowseQuery) -> Result<MediaPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let response = self
            .send(&server_id, client.browse_media(browse_params(&query)?))
            .await?;
        Ok(MediaPage::from_generated(response.into_inner(), &record))
    }

    /// Everything a detail screen shows for one title.
    ///
    /// Read from the server every time, never cached: the detail carries the
    /// viewer's watched state (issue #188), which playback and a watched mark
    /// change under it, so a cached copy would show an episode just finished
    /// as unwatched.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn media_detail(&self, media_id: String) -> Result<MediaDetail, BeamError> {
        let wire_media_id = parse_uuid("media_id", &media_id)?;
        let (server_id, client, record) = self.active_context()?;
        let response = self
            .send(&server_id, client.get_media_detail(wire_media_id))
            .await?;
        Ok(MediaDetail::from_generated(response.into_inner(), &record))
    }

    /// Mark a movie, an episode, a season or a whole show watched.
    ///
    /// A season or a show marks each of its episodes. Marking a title already
    /// watched changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `id` is not a UUID and
    /// [`BeamError::NotFound`] for an id the server does not know.
    pub async fn mark_watched(&self, id: String) -> Result<(), BeamError> {
        let wire_id = parse_uuid("id", &id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.mark_watched(wire_id)).await?;
        Ok(())
    }

    /// Mark a movie, an episode, a season or a whole show unwatched: its
    /// played state, play count and resume position are all forgotten.
    ///
    /// # Errors
    ///
    /// As [`Self::mark_watched`].
    pub async fn mark_unwatched(&self, id: String) -> Result<(), BeamError> {
        let wire_id = parse_uuid("id", &id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.mark_unwatched(wire_id))
            .await?;
        Ok(())
    }

    /// Every genre the catalog contains, for the explore filters.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn genres(&self) -> Result<Vec<String>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_genres()).await?;
        // A connection that is never paged: `items` is every genre.
        Ok(response.into_inner().items)
    }

    /// The episode of `show_id` that plays after `current_episode_id`.
    ///
    /// The server's answer, `EpisodeDetail::next_episode_id` (issue #188),
    /// rather than one worked out here: next-up skips episodes with no file,
    /// crosses season boundaries, and steps over the run of episodes a
    /// multi-episode file holds, and only the server knows the last of those.
    /// So every client advances to the same episode.
    ///
    /// `None` after the last episode, and for an episode that is not
    /// `show_id`'s: auto-advance never carries a viewer into another show.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] for an identifier that is not a UUID,
    /// and propagates transport and server failures.
    pub async fn up_next_in_show(
        &self,
        show_id: String,
        current_episode_id: String,
    ) -> Result<Option<EpisodeSummary>, BeamError> {
        let wire_show_id = parse_uuid("show_id", &show_id)?;
        let wire_current_id = parse_uuid("current_episode_id", &current_episode_id)?;
        let (server_id, client, record) = self.active_context()?;

        let current = self
            .send(&server_id, client.get_episode_detail(wire_current_id))
            .await?
            .into_inner();
        if current.show.id != wire_show_id {
            return Ok(None);
        }
        let Some(next_id) = current.next_episode_id else {
            return Ok(None);
        };
        let next = self
            .send(&server_id, client.get_episode_detail(next_id))
            .await?
            .into_inner();
        Ok(Some(EpisodeSummary::from_generated(&next.episode, &record)))
    }

    /// Every library on the server.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn libraries(&self) -> Result<Vec<LibrarySummary>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_libraries()).await?;
        // A connection that is never paged: `items` is every library.
        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(LibrarySummary::from_generated)
            .collect())
    }

    /// One library.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn library(&self, library_id: String) -> Result<LibrarySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(
                &server_id,
                client.get_library(parse_uuid("library_id", &library_id)?),
            )
            .await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// The files indexed into one library, by path.
    ///
    /// The server pages this list; the core walks every page, at the
    /// largest size the server takes, so a caller still gets the whole list.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn library_files(
        &self,
        library_id: String,
    ) -> Result<Vec<LibraryFileSummary>, BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        let mut files = Vec::new();
        let mut after = None;
        loop {
            let params = crate::api::GetLibraryFilesParams {
                first: Some(LIBRARY_FILES_PAGE),
                after: after.take(),
            };
            let page = self
                .send(
                    &server_id,
                    client.get_library_files(wire_library_id, params),
                )
                .await?
                .into_inner();
            files.extend(
                page.items
                    .into_iter()
                    .map(LibraryFileSummary::from_generated),
            );
            match page.page_info.end_cursor {
                Some(cursor) if page.page_info.has_next_page => after = Some(cursor),
                _ => break,
            }
        }
        Ok(files)
    }

    /// Titles to resume and next episodes to start, newest first.
    ///
    /// One round trip: every row carries what it displays (issue #188), so
    /// nothing is looked up per row.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn continue_watching(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<ContinueWatchingEntry>, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetContinueWatchingParams {
            first: limit.map(i64::from),
        };
        let response = self
            .send(&server_id, client.get_continue_watching(params))
            .await?;

        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(|item| ContinueWatchingEntry::from_generated(item, &record))
            .collect())
    }

    /// One page of watch history, newest first: `first` rows, after the
    /// page whose `end_cursor` is `after`, or from the start.
    ///
    /// Each row carries what it displays, as [`Self::continue_watching`]'s
    /// do.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn history(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<HistoryPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetHistoryParams {
            first: first.map(i64::from),
            after,
        };
        let response = self.send(&server_id, client.get_history(params)).await?;
        Ok(HistoryPage::from_generated(response.into_inner(), &record))
    }
}
