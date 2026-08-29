//! MIGRATION SCAFFOLD -- only the modules already ported to Kynos are wired
//! here. The full route table is restored as each module lands.

pub mod api_error;
pub mod genres;
pub mod media;
pub mod health;
pub mod tags;

pub use genres::*;
pub use media::*;
pub use health::*;

use kynos::prelude::*;

use crate::state::AppState;

/// Every `/v1` operation, as one table.
pub fn rest_routes() -> Router<AppState> {
    Router::new().mount(kynos::routes![
        health::health_check,
        genres::list_genres,
        media::browse_media,
        media::get_media_detail,
        media::get_media_sources,
    ])
}

/// The router the process serves and the document is derived from.
pub fn create_router() -> Router<AppState> {
    Router::new().nest("/v1", rest_routes())
}
