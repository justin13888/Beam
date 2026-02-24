pub mod graphql;
pub mod graphql_ws;
pub mod health;
pub mod stream;
pub mod upload;

use salvo::prelude::*;
use std::sync::Arc;

pub use health::*;
pub use stream::*;

use beam_auth::utils::service::AuthService;
use beam_stream::graphql::AppSchema;
use beam_stream::state::AppState;

/// Create the main API router with all routes
pub fn create_router(state: AppState, schema: AppSchema) -> Router {
    let auth_service: Arc<dyn AuthService> = Arc::clone(&state.services.auth);

    // Note: No authorization is done at the top-level here because only `graphql` is secured with auth the other endpoints are either public or require query params (i.e., presigned URLs)
    Router::new().hoop(affix_state::inject(state)).push(
        Router::with_path("v1")
            .push(Router::with_path("health").get(health_check))
            .push(Router::with_path("stream/<id>/token").post(get_stream_token))
            .push(Router::with_path("stream/mp4/<id>").get(stream_mp4))
            .push(
                Router::with_path("auth").push(beam_auth::server::auth_routes()),
            )
            .push(
                Router::with_path("graphql")
                    .hoop(affix_state::inject(schema.clone()))
                    .get(graphql::graphiql)
                    .post(graphql::graphql_handler),
            )
            .push(
                Router::with_path("graphql/ws")
                    .hoop(affix_state::inject(schema))
                    .get(graphql_ws::graphql_ws_handler),
            ),
    )
}
