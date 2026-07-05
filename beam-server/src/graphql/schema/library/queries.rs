use async_graphql::*;

use crate::graphql::AuthGuard;
use crate::models::{Library, LibraryFile};
use crate::state::{AppContext, AppState};

#[derive(Default)]
pub struct LibraryQuery;

#[Object]
impl LibraryQuery {
    /// Fetch list of all libraries
    #[graphql(guard = "AuthGuard")]
    async fn libraries(&self, ctx: &Context<'_>) -> Result<Vec<Library>> {
        let state = ctx.data::<AppState>()?;
        let context = ctx.data::<AppContext>()?;
        let user_ctx = context
            .user_context()
            .ok_or_else(|| Error::new("Unauthorized"))?;

        let libraries = state
            .services
            .library
            .get_libraries(user_ctx.user_id.clone())
            .await?;

        Ok(libraries)
    }

    /// Fetch a single library by its ID
    #[graphql(guard = "AuthGuard")]
    async fn library_by_id(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Library>> {
        let state = ctx.data::<AppState>()?;
        let library = state
            .services
            .library
            .get_library_by_id(id.to_string())
            .await?;
        Ok(library)
    }

    /// Fetch all files within a library
    #[graphql(guard = "AuthGuard")]
    async fn library_files(&self, ctx: &Context<'_>, library_id: ID) -> Result<Vec<LibraryFile>> {
        let state = ctx.data::<AppState>()?;
        let files = state
            .services
            .library
            .get_library_files(library_id.to_string())
            .await?;
        Ok(files)
    }
}
