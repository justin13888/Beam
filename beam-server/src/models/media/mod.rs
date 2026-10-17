use kynos::Schema;
use serde::Serialize;

mod artwork;
mod movie;
mod show;
mod source;

pub use artwork::*;
pub use movie::*;
pub use show::*;
pub use source::*;

/// Media metadata
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub enum MediaMetadata {
    Show(ShowMetadata),
    Movie(MovieMetadata),
}

impl MediaMetadata {
    pub fn title(&self) -> &Title {
        match self {
            MediaMetadata::Show(s) => &s.title,
            MediaMetadata::Movie(m) => &m.title,
        }
    }
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct Title {
    /// Original title
    pub original: String,
    /// Localized title, if available and different from original
    pub localized: Option<String>,
    /// Alternative titles, if any
    pub alternatives: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct ExternalIdentifiers {
    /// IMDb ID (e.g., tt1234567)
    pub imdb_id: Option<String>,
    /// TMDb ID (e.g., 12345)
    pub tmdb_id: Option<u32>,
    /// TVDb ID (e.g., 12345)
    pub tvdb_id: Option<u32>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct Ratings {
    /// TMDB rating on TMDB's own 0-10 scale, as precise as it is stored.
    #[schema(minimum = 0, maximum = 10)]
    pub tmdb: Option<f64>,
    // TODO: Add more ratings sources if needed
}
