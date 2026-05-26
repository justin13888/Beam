use async_graphql::*;

use crate::graphql::AuthGuard;
use crate::services::metadata::MediaFilter;
use crate::state::AppState;

#[derive(Default)]
pub struct MediaMutation;

#[Object]
impl MediaMutation {
    /// Refresh media metadata by ID
    #[graphql(guard = "AuthGuard")]
    async fn refresh_metadata(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let state = ctx.data::<AppState>()?;
        state
            .services
            .metadata
            .refresh_metadata(MediaFilter::ByMediaId(id.to_string()))
            .await?;
        Ok(true)
    }
}
