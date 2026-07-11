pub mod admin;
pub mod api_error;
pub mod genres;
pub mod health;
pub mod media;
pub mod metrics_mw;
pub mod middleware;
pub mod playback;
pub mod rate_limit;
pub mod stream;
#[cfg(test)]
pub(crate) mod test_support;

use metrics_exporter_prometheus::PrometheusHandle;
use salvo::prelude::*;

pub use admin::*;
pub use genres::*;
pub use health::*;
pub use media::*;
pub use playback::*;
pub use stream::*;

use crate::config::ServerConfig;
use crate::state::AppState;
use rate_limit::RateLimiters;

/// Builds the rate limiters to install, or `None` when rate limiting is
/// disabled (`BEAM_RATE_LIMIT_ENABLED=false`) so no hoops are mounted at all.
fn build_rate_limiters(config: &ServerConfig) -> Option<RateLimiters> {
    config
        .rate_limit_enabled
        .then(|| RateLimiters::from_config(config))
}

/// REST-only sub-routes (health, stream, media, libraries, admin, auth).
/// Single source of truth used by both `create_router` and
/// `create_docs_router` so new endpoints only need to be registered in one
/// place.
///
/// `rate_limiters` is `Some` only for the live server (`create_router`); the
/// docs router passes `None` so the rate-limit hoops never appear there and the
/// exported OpenAPI spec stays byte-for-byte unchanged.
fn rest_routes(rate_limiters: Option<RateLimiters>) -> Router {
    // Pull the two rate-limited subrouters out of the chain so their limiter
    // hoops can be attached here (and only here). Split first so each limiter
    // moves into exactly one router.
    let (auth_limiter, search_limiter) = match rate_limiters {
        Some(RateLimiters { auth, search }) => (Some(auth), Some(search)),
        None => (None, None),
    };

    // SEARCH class: only the browse/search endpoint. `media/{id}` and
    // `media/{id}/sources` are separate routers below and stay unlimited.
    let mut media_browse = Router::with_path("media").get(browse_media);
    if let Some(search) = search_limiter {
        media_browse = media_browse.hoop(search);
    }

    // AUTH class: the whole `/auth` subtree (login + callback).
    // OIDC login/callback stay under /auth; everything else that acts on the
    // resulting session cookie is top-level, matching the final ratified shape
    // (see ADR-0003) now that no legacy auth routes remain to coexist with.
    let mut auth_router = Router::with_path("auth")
        .push(Router::with_path("login").get(beam_auth::server::oidc_login))
        .push(Router::with_path("callback").get(beam_auth::server::oidc_callback));
    if let Some(auth) = auth_limiter {
        auth_router = auth_router.hoop(auth);
    }

    Router::new()
        .push(Router::with_path("health").get(health_check))
        .push(media_browse)
        .push(Router::with_path("media/{id}").get(get_media_detail))
        .push(Router::with_path("media/{id}/sources").get(get_media_sources))
        .push(Router::with_path("genres").get(list_genres))
        .push(Router::with_path("libraries").get(list_libraries))
        .push(Router::with_path("libraries/{id}").get(get_library))
        .push(Router::with_path("libraries/{id}/files").get(get_library_files))
        .push(Router::with_path("files/{file_id}/progress").put(report_playback_progress))
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
        .push(auth_router)
        .push(Router::with_path("me").get(beam_auth::server::oidc_me))
        .push(Router::with_path("logout").post(beam_auth::server::oidc_logout))
        .push(Router::with_path("logout-all").post(beam_auth::server::oidc_logout_all))
        .push(Router::with_path("sessions").get(beam_auth::server::oidc_list_sessions))
        .push(Router::with_path("sessions/{id}").delete(beam_auth::server::oidc_delete_session))
}

/// Create the main API router with all routes.
///
/// `metrics_handle` is `Some` when `BEAM_ENABLE_METRICS=true` (main installs
/// the Prometheus recorder and passes its handle here). It switches on both
/// metrics pieces at once: the [`metrics_mw::HttpMetrics`] hoop wrapping the
/// `/v1` subtree, and the top-level unauthenticated `GET /metrics` exposition
/// route. When `None`, neither is mounted — zero overhead, matching the
/// flag's promise. `/metrics` never appears in `create_docs_router`, so the
/// exported OpenAPI spec is independent of the flag.
pub fn create_router(state: AppState, metrics_handle: Option<PrometheusHandle>) -> Router {
    // Note: No authorization is done at the top-level here -- each endpoint is
    // either public or self-contained (admin routes gated via
    // `require_admin`).
    //
    // The `beam_auth::server::oidc_*` handlers (mounted individually above)
    // pull their dependencies straight from the depot via
    // `depot.obtain::<Arc<dyn ...>>()`, so they're injected here individually
    // rather than only injecting the outer `AppState`.
    let services = &state.services;
    let user_repo = services.user_repo.clone();
    let session_store = services.session_store.clone();
    let oidc_client = services.oidc_client.clone();
    let pending_auth_store = services.pending_auth_store.clone();
    let oidc_config = services.oidc_config.clone();

    // Build limiters (or not) before `state` is moved into the affix hoop.
    let rate_limiters = build_rate_limiters(&state.config);

    let mut v1 = Router::with_path("v1");
    if metrics_handle.is_some() {
        // First hoop on the subtree = outermost: it observes the final status
        // of every /v1 response, including 429s from the rate limiters and
        // same-origin rejections below.
        v1 = v1.hoop(metrics_mw::HttpMetrics);
    }
    let v1 = v1
        .hoop(middleware::enforce_same_origin)
        .push(rest_routes(rate_limiters));

    let mut router = Router::new()
        .hoop(affix_state::inject(state))
        .hoop(affix_state::inject(user_repo))
        .hoop(affix_state::inject(session_store))
        .hoop(affix_state::inject(oidc_client))
        .hoop(affix_state::inject(pending_auth_store))
        .hoop(affix_state::inject(oidc_config))
        .push(v1);

    if let Some(handle) = metrics_handle {
        router =
            router.push(Router::with_path("metrics").get(metrics_mw::MetricsEndpoint::new(handle)));
    }

    router
}

/// Create a minimal router for OpenAPI documentation export.
///
/// Includes only the REST endpoints (health, stream, auth) without state
/// injection middleware.
pub fn create_docs_router() -> Router {
    // `None`: no rate-limit hoops in the docs router, keeping the exported
    // OpenAPI spec unchanged.
    Router::new().push(Router::with_path("v1").push(rest_routes(None)))
}
