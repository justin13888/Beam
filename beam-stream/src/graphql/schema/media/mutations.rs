use async_graphql::*;

use crate::graphql::AuthGuard;
#[derive(Default)]
pub struct MediaMutation;

#[Object]
impl MediaMutation {
    /// Refresh media metadata by ID
    #[graphql(guard = "AuthGuard")]
    async fn refresh_metadata(&self, _ctx: &Context<'_>, _id: ID) -> Result<bool> {
        todo!()
    }
}
