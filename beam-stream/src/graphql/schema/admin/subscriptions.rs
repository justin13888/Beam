use async_graphql::*;
use async_stream::stream;
use tokio::sync::broadcast::error::RecvError;

use crate::graphql::AuthGuard;
use crate::services::notification::AdminEvent;
use crate::state::AppState;

#[derive(Default)]
pub struct AdminSubscription;

#[Subscription]
impl AdminSubscription {
    /// Subscribe to real-time admin events.
    /// Yields events as they are published (library scans, warnings, errors, etc.).
    #[graphql(guard = "AuthGuard")]
    async fn admin_events_watch(
        &self,
        ctx: &Context<'_>,
    ) -> impl futures_util::Stream<Item = AdminEvent> + use<'_> {
        let state = ctx.data_unchecked::<AppState>();
        let mut receiver = state.services.notification.subscribe();

        stream! {
            loop {
                match receiver.recv().await {
                    Ok(event) => yield event,
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "Admin events subscription lagged, skipped {} events",
                            skipped
                        );
                        // Continue receiving; consumer will miss skipped events
                    }
                }
            }
        }
    }
}
