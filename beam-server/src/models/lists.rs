//! Wire shapes for the watchlist and favourites (issue #262).

use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::{Deserialize, Serialize};

use crate::models::media::MediaMetadata;
use crate::models::search::PageInfo;

/// One title on the viewer's watchlist or among their favourites.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct UserListItem {
    /// When the viewer added it: the list is newest first by this.
    pub added_at: DateTime<Utc>,
    /// The movie or show, as a browse result lists it, with the viewer's
    /// state for a movie.
    pub media: MediaMetadata,
}

/// One page of the viewer's watchlist or favourites, newest addition first.
///
/// Pass `page_info.end_cursor` as `after` for the next page. `total` counts
/// every title on the list.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct UserListConnection {
    pub items: Vec<UserListItem>,
    pub page_info: PageInfo,
    pub total: u64,
}
