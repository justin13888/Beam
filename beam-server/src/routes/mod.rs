//! MIGRATION SCAFFOLD -- only the modules already ported to Kynos are wired
//! here. The full route table is restored as each module lands.

pub mod admin;
pub mod api_error;
pub mod auth;
pub mod genres;
pub mod media;
pub mod playback;
pub mod stream;
pub mod health;
pub mod tags;

pub use admin::*;
pub use auth::*;
pub use genres::*;
pub use media::*;
pub use playback::*;
pub use stream::*;
pub use health::*;

use kynos::prelude::*;

use crate::state::AppState;

/// Every `/v1` operation, as one table.
///
/// One `mount` per module rather than one list: `routes!` builds a tuple, and
/// the arity runs out well before Beam's operation count. Grouping by module is
/// what the split would have been anyway.
pub fn rest_routes() -> Router<AppState> {
    Router::new()
        .mount(kynos::routes![health::health_check])
        .mount(kynos::routes![genres::list_genres])
        .mount(kynos::routes![
            media::browse_media,
            media::get_media_detail,
            media::get_media_sources,
        ])
        .mount(kynos::routes![
            playback::report_playback_progress,
            playback::get_continue_watching,
            playback::get_history,
        ])
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
        ])
        .mount(kynos::routes![
            stream::stream_file,
            stream::head_stream_file,
            stream::download_file,
            stream::head_download_file,
        ])
        .mount(kynos::routes![
            auth::oidc_login,
            auth::oidc_callback,
            auth::oidc_me,
            auth::oidc_logout,
            auth::oidc_logout_all,
            auth::oidc_list_sessions,
            auth::oidc_delete_session,
        ])
}

/// The router the process serves and the document is derived from.
pub fn create_router() -> Router<AppState> {
    Router::new().nest("/v1", rest_routes())
}
