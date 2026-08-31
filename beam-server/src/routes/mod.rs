//! The `/v1` route table, and the router the process serves.

pub mod admin;
pub mod api_error;
pub mod auth;
pub mod genres;
pub mod health;
pub mod media;
pub mod metrics_mw;
pub mod middleware;
pub mod playback;
pub mod rate_limit;
pub mod stream;
pub mod tags;

// No `pub use <module>::*`. Two modules declare a `FilePath` -- `playback` for
// progress reporting and `stream` for delivery -- and glob re-exporting both
// made the name ambiguous at this level. Nothing outside this module used the
// globs, and Kynos's own convention is that every item has one canonical path,
// so they are gone rather than disambiguated.

use kynos::middleware::rate_limit::RateLimit;
use kynos::openapi::Info;
use kynos::prelude::*;
use kynos::router::docs::Docs;

use crate::bootstrap;
use crate::routes::rate_limit::{BeamRateLimit, Class};
use crate::state::AppState;

#[cfg(test)]
#[path = "test_support.rs"]
pub(crate) mod test_support;

/// Every `/v1` operation, as one table.
///
/// Grouped by tag rather than mounted flat. Every route here also carries a
/// `tag = ...` in its own attribute, which reads like the tag is set there --
/// but Kynos 0.1.0 never applies it: `Router::describe` unions the router's and
/// the group's tag scopes and does not read the endpoint's own `TAGS` const, so
/// a route-level tag is accepted by the macro and silently dropped. Every tag
/// disappeared from the exported document when the port first ran.
///
/// So the tags are declared where Kynos does read them. The route attributes
/// keep theirs as the statement of intent, and the grouping is one group per
/// tag, which is the structure Kynos recommends anyway. Filed upstream; when it
/// lands, the groups that exist only to carry a tag can collapse back.
///
/// One `mount` per module rather than one list: `routes!` builds a tuple, and
/// the arity runs out well before Beam's operation count. Grouping by module is
/// what the split would have been anyway.
pub fn rest_routes() -> Router<AppState> {
    Router::new()
        .group(
            Group::new("/")
                .tag::<tags::Health>()
                .mount(kynos::routes![health::health_check]),
        )
        .group(
            Group::new("/")
                .tag::<tags::Media>()
                .mount(kynos::routes![genres::list_genres])
                .mount(kynos::routes![
                    media::get_media_detail,
                    media::get_media_sources,
                ]),
        )
        // `browse_media` fans out into metadata queries, so it is the most
        // expensive read path and the one worth a budget. Its own group: two
        // `RateLimit`s on one operation would both declare a 429 and both add
        // `X-RateLimit-*`, which Kynos refuses at build time.
        .group(
            Group::new("/")
                .tag::<tags::Media>()
                .mount(kynos::routes![media::browse_media])
                .intercept(RateLimit::new(BeamRateLimit::new(Class::Search))),
        )
        .group(
            Group::new("/")
                .tag::<tags::Playback>()
                .mount(kynos::routes![
                    playback::report_playback_progress,
                    playback::get_continue_watching,
                    playback::get_history,
                ])
                .mount(kynos::routes![
                    stream::stream_file,
                    stream::head_stream_file,
                    stream::download_file,
                    stream::head_download_file,
                ]),
        )
        .group(
            Group::new("/")
                .tag::<tags::Admin>()
                .mount(kynos::routes![
                    admin::list_libraries,
                    admin::get_library,
                    admin::get_library_files,
                    admin::create_library,
                    admin::scan_library,
                    admin::refresh_media_metadata,
                    admin::delete_library,
                ])
                .mount(kynos::routes![
                    admin::get_admin_logs,
                    admin::get_admin_log_count,
                    admin::get_admin_events,
                    admin::stream_admin_events,
                    admin::list_admin_users,
                    admin::update_admin_user,
                    admin::get_admin_status,
                ]),
        )
        .group(Group::new("/").tag::<tags::Auth>().mount(kynos::routes![
            auth::oidc_me,
            auth::oidc_logout,
            auth::oidc_logout_all,
            auth::oidc_list_sessions,
            auth::oidc_delete_session,
        ]))
        // The two operations that begin an OIDC flow, sharing one budget --
        // which is what makes the auth class a class. Keyed by client only, so
        // spending the budget on `login` also spends it for `callback`.
        .group(
            Group::new("/")
                .tag::<tags::Auth>()
                .mount(kynos::routes![auth::oidc_login, auth::oidc_callback])
                .intercept(RateLimit::new(BeamRateLimit::new(Class::Auth))),
        )
}

/// The router the process serves and the document is derived from.
///
/// Takes no arguments on purpose. Kynos derives the dispatch table and the
/// OpenAPI document from one walk of this value, so anything passed in at
/// startup would be something the exported description cannot see -- and a
/// served surface that disagrees with its document is the failure ADR-0010
/// exists to remove. Everything that used to arrive as an argument (the
/// Prometheus handle, the rate-limit numbers, the clock) is read off
/// `AppState` at request time instead.
///
/// The CORS policy and the same-origin check are mounted in one expression
/// because they are one control. `bootstrap::cors_policy` mirrors the request
/// origin and allows credentials, which on its own would let any site read an
/// authenticated response; it is safe only because `EnforceSameOrigin` rejects
/// a cross-origin write before it reaches a handler. Dropping either is a
/// one-line diff sitting next to the comment that says why it cannot be.
///
/// CORS is listed first so it is the outer of the two: a rejected cross-origin
/// write still leaves with `Access-Control-Allow-Origin`, so the browser
/// reports the real 403 rather than an opaque network error.
///
/// Neither reaches `/metrics` or the docs pages. They are mounted at the root,
/// and Kynos copies a nested router's interceptors onto that router's own
/// operations only -- which is what we want: a scrape target has no session to
/// forge, and a cross-origin reader of `/openapi` has nothing to steal.
pub fn create_router() -> Router<AppState> {
    Router::new()
        .info(Info::new("Beam Server API", "1.0.0"))
        .nest(
            "/v1",
            rest_routes()
                .intercept(bootstrap::cors_policy())
                .intercept(middleware::EnforceSameOrigin),
        )
        .group(
            Group::new("/")
                .tag::<tags::Internal>()
                .mount(kynos::routes![metrics_mw::render_metrics]),
        )
        .observe(metrics_mw::HttpMetrics)
        .docs(
            Docs::scalar()
                .at("/openapi")
                .description_at("/api-doc/openapi.json"),
        )
}
