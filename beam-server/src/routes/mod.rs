pub mod admin;
pub mod api_error;
pub mod graphql;
pub mod graphql_ws;
pub mod health;
pub mod media;
pub mod stream;

use salvo::prelude::*;

pub use admin::*;
pub use health::*;
pub use media::*;
pub use stream::*;

use crate::graphql::AppSchema;
use crate::state::AppState;

/// REST-only sub-routes (health, stream, media, libraries, admin, auth).
/// Single source of truth used by both `create_router` and
/// `create_docs_router` so new endpoints only need to be registered in one
/// place.
fn rest_routes() -> Router {
    Router::new()
        .push(Router::with_path("health").get(health_check))
        .push(Router::with_path("stream/{id}/token").post(get_stream_token))
        .push(Router::with_path("stream/mp4/{id}").get(stream_mp4))
        .push(Router::with_path("media").get(browse_media))
        .push(Router::with_path("media/{id}").get(get_media_detail))
        .push(Router::with_path("media/{id}/sources").get(get_media_sources))
        .push(Router::with_path("libraries").get(list_libraries))
        .push(Router::with_path("libraries/{id}").get(get_library))
        .push(Router::with_path("libraries/{id}/files").get(get_library_files))
        .push(
            Router::with_path("admin")
                .push(Router::with_path("libraries").post(create_library))
                .push(Router::with_path("libraries/{id}/scan").post(scan_library))
                .push(Router::with_path("libraries/{id}").delete(delete_library))
                .push(Router::with_path("media/{id}/refresh").post(refresh_media_metadata))
                .push(Router::with_path("logs").get(get_admin_logs))
                .push(Router::with_path("logs/count").get(get_admin_log_count))
                .push(Router::with_path("events").get(get_admin_events))
                .push(Router::with_path("events/stream").get(stream_admin_events)),
        )
        .push(Router::with_path("auth").push(beam_auth::server::auth_routes()))
}

/// Create the main API router with all routes
pub fn create_router(state: AppState, schema: AppSchema) -> Router {
    // Note: No authorization is done at the top-level here because only `graphql` is secured with auth; other endpoints are either public or self-contained (e.g., stream token validated in the handler).
    Router::new().hoop(affix_state::inject(state)).push(
        Router::with_path("v1")
            .push(rest_routes())
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

/// Create a minimal router for OpenAPI documentation export.
///
/// Includes only the REST endpoints (health, stream, auth) without state
/// injection middleware or GraphQL routes (which use `#[handler]` and
/// contribute nothing to the OpenAPI spec).
pub fn create_docs_router() -> Router {
    Router::new().push(Router::with_path("v1").push(rest_routes()))
}
