use async_graphql::*;

use crate::models::MediaMetadata;

#[derive(Default)]
pub struct MediaQuery;

// Re-export types from services for GraphQL
use crate::services::{
    MediaConnection, MediaSearchFilters, MediaSortField, MediaTypeFilter, SortOrder,
};

use crate::graphql::AuthGuard;
use crate::state::AppState;

#[Object]
impl MediaQuery {
    /// Fetch media metadata by ID
    #[graphql(guard = "AuthGuard")]
    async fn metadata(&self, ctx: &Context<'_>, id: ID) -> Result<Option<MediaMetadata>> {
        let state = ctx.data::<AppState>()?;
        let media_metadata = state.services.metadata.get_media_metadata(&id).await;

        Ok(media_metadata)
    }

    /// Search/explore media with cursor-based pagination (Relay-style), sorting, and filtering
    #[allow(clippy::too_many_arguments)]
    #[graphql(guard = "AuthGuard")]
    async fn search(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Number of items to return from the start")] first: Option<u32>,
        #[graphql(desc = "Cursor to start after")] after: Option<String>,
        #[graphql(desc = "Number of items to return from the end")] last: Option<u32>,
        #[graphql(desc = "Cursor to start before")] before: Option<String>,
        #[graphql(desc = "Sort field", default)] sort_by: MediaSortField,
        #[graphql(desc = "Sort order", default)] sort_order: SortOrder,
        #[graphql(desc = "Filter by media type")] media_type: Option<MediaTypeFilter>,
        #[graphql(desc = "Filter by genre")] genre: Option<String>,
        #[graphql(desc = "Filter by year (exact match)")] year: Option<u32>,
        #[graphql(desc = "Filter by year range (start)")] year_from: Option<u32>,
        #[graphql(desc = "Filter by year range (end)")] year_to: Option<u32>,
        #[graphql(desc = "Search query for title")] query: Option<String>,
        #[graphql(desc = "Filter by minimum rating (0-100)")] min_rating: Option<u32>,
    ) -> Result<MediaConnection> {
        let state = ctx.data::<AppState>()?;

        let filters = MediaSearchFilters {
            media_type,
            genre,
            year,
            year_from,
            year_to,
            query,
            min_rating,
        };

        let result = state
            .services
            .metadata
            .search_media(first, after, last, before, sort_by, sort_order, filters)
            .await;

        Ok(result)
    }
}
