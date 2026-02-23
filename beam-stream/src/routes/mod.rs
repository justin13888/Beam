pub mod auth;
pub mod graphql;
pub mod health;
pub mod stream;
pub mod upload;

use salvo::prelude::*;

pub use health::*;
pub use stream::*;

use beam_stream::graphql::AppSchema;
use beam_stream::state::AppState;

/// Create the main API router with all routes
pub fn create_router(state: AppState, schema: AppSchema) -> Router {
    // Note: No authorization is done at the top-level here because only `graphql` is secured with auth the other endpoints are either public or require query params (i.e., presigned URLs)
    Router::new().hoop(affix_state::inject(state)).push(
        Router::with_path("v1")
            .push(Router::with_path("health").get(health_check))
            .push(Router::with_path("stream/<id>/token").post(get_stream_token))
            .push(Router::with_path("stream/mp4/<id>").get(stream_mp4))
            .push(Router::with_path("auth").push(auth::auth_routes()))
            .push(
                Router::with_path("graphql")
                    .hoop(affix_state::inject(schema))
                    .get(graphql::graphiql)
                    .post(graphql::graphql_handler),
            ),
    )
}
