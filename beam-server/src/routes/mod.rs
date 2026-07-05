pub mod admin;
pub mod api_error;
pub mod health;
pub mod media;
pub mod middleware;
pub mod playback;
pub mod stream;

use salvo::prelude::*;

pub use admin::*;
pub use health::*;
pub use media::*;
pub use playback::*;
pub use stream::*;

use crate::state::AppState;

/// REST-only sub-routes (health, stream, media, libraries, admin, auth).
/// Single source of truth used by both `create_router` and
/// `create_docs_router` so new endpoints only need to be registered in one
/// place.
fn rest_routes() -> Router {
    Router::new()
        .push(Router::with_path("health").get(health_check))
        .push(Router::with_path("media").get(browse_media))
        .push(Router::with_path("media/{id}").get(get_media_detail))
        .push(Router::with_path("media/{id}/sources").get(get_media_sources))
        .push(Router::with_path("libraries").get(list_libraries))
        .push(Router::with_path("libraries/{id}").get(get_library))
        .push(Router::with_path("libraries/{id}/files").get(get_library_files))
        .push(Router::with_path("files/{file_id}/progress").put(report_playback_progress))
        .push(Router::with_path("files/{file_id}/stream-token").post(get_stream_token))
        .push(Router::with_path("files/{file_id}/stream").get(stream_file))
        .push(Router::with_path("files/{file_id}/download").get(download_file))
        .push(Router::with_path("continue-watching").get(get_continue_watching))
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
pub fn create_router(state: AppState) -> Router {
    // Note: No authorization is done at the top-level here -- each endpoint is
    // either public or self-contained (e.g. stream token validated in the
    // handler, admin routes gated via `require_admin`).
    //
    // `beam_auth::server::auth_routes()` (mounted inside `rest_routes()`)
    // pulls its dependencies straight from the depot via
    // `depot.obtain::<Arc<dyn ...>>()`, so they're injected here individually
    // rather than only injecting the outer `AppState`.
    let services = &state.services;
    let auth = services.auth.clone();
    let user_repo = services.user_repo.clone();
    let session_store = services.session_store.clone();
    let oidc_client = services.oidc_client.clone();
    let pending_auth_store = services.pending_auth_store.clone();
    let oidc_config = services.oidc_config.clone();

    Router::new()
        .hoop(affix_state::inject(state))
        .hoop(affix_state::inject(auth))
        .hoop(affix_state::inject(user_repo))
        .hoop(affix_state::inject(session_store))
        .hoop(affix_state::inject(oidc_client))
        .hoop(affix_state::inject(pending_auth_store))
        .hoop(affix_state::inject(oidc_config))
        .push(
            Router::with_path("v1")
                .hoop(middleware::enforce_same_origin)
                .push(rest_routes()),
        )
}

/// Create a minimal router for OpenAPI documentation export.
///
/// Includes only the REST endpoints (health, stream, auth) without state
/// injection middleware.
pub fn create_docs_router() -> Router {
    Router::new().push(Router::with_path("v1").push(rest_routes()))
}
