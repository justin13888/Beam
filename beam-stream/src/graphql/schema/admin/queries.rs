use async_graphql::*;

use crate::graphql::AuthGuard;
use crate::services::notification::AdminEvent;
use crate::state::AppState;

#[derive(Default)]
pub struct AdminQuery;

#[Object]
impl AdminQuery {
    /// Fetch recent admin events from the event log.
    /// Returns the most recent `limit` events (default 100, max 1000).
    #[graphql(guard = "AuthGuard")]
    async fn admin_events(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u32,
    ) -> Result<Vec<AdminEvent>> {
        let state = ctx.data::<AppState>()?;
        let limit = (limit as usize).min(1000);
        let events = state.services.notification.recent_events(limit);
        Ok(events)
    }
}
