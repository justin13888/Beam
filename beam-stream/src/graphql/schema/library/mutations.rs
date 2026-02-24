use async_graphql::*;

use crate::graphql::AuthGuard;
use crate::models::Library;
use crate::services::metadata::MediaFilter;
use crate::state::AppState;

#[derive(Default)]
pub struct LibraryMutation;

#[Object]
impl LibraryMutation {
    /// Refresh metadata for media library
    #[graphql(guard = "AuthGuard")]
    async fn refresh_metadata(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let state = ctx.data::<AppState>()?;
        state
            .services
            .metadata
            .refresh_metadata(MediaFilter::ByLibraryId(id.to_string()))
            .await?;
        Ok(true)
    }

    /// Create a new library
    #[graphql(guard = "AuthGuard")]
    async fn create_library(
        &self,
        ctx: &Context<'_>,
        name: String,
        root_path: String,
    ) -> Result<Library> {
        let state = ctx.data::<AppState>()?;
        let library = state
            .services
            .library
            .create_library(name, root_path)
            .await?;
        Ok(library)
    }

    /// Scan a library for new content
    #[graphql(guard = "AuthGuard")]
    async fn scan_library(&self, ctx: &Context<'_>, id: ID) -> Result<u32> {
        let state = ctx.data::<AppState>()?;
        let count = state.services.library.scan_library(id.to_string()).await?;
        Ok(count)
    }

    /// Delete a library and all its associated files
    #[graphql(guard = "AuthGuard")]
    async fn delete_library(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let state = ctx.data::<AppState>()?;
        let deleted = state
            .services
            .library
            .delete_library(id.to_string())
            .await?;
        Ok(deleted)
    }
}
