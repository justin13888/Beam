//! How artwork is addressed.
//!
//! Shared by the endpoint that serves it and the metadata service that points
//! clients at it: one spelling of `movie`/`poster` rather than two that drift.

use kynos::Schema;
use serde::{Deserialize, Serialize};

/// Which kind of title the artwork belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkKind {
    /// A movie.
    Movie,
    /// A show.
    Show,
    /// One season of a show.
    Season,
    /// One episode of a season.
    Episode,
}

/// Which image of that title.
///
/// Not every pairing exists -- an episode has a thumbnail and no poster, a
/// season has a poster and no backdrop -- and the ones that do not are a 404,
/// the same answer as a title that simply has no art yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkVariant {
    /// Portrait cover art.
    Poster,
    /// Landscape background art.
    Backdrop,
    /// An episode still.
    Thumbnail,
}

/// A path segment that names no artwork Beam serves.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("unrecognised artwork path segment")]
pub struct UnknownArtworkSegment;

// `Display` and `FromStr` rather than the serde pair: Kynos parses and renders
// a path parameter through those, and having one spelling of these variants
// rather than two is what keeps the route, the description and the generated
// clients agreeing on what `/artwork/movie/{id}/poster` means.
impl std::fmt::Display for ArtworkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Movie => "movie",
            Self::Show => "show",
            Self::Season => "season",
            Self::Episode => "episode",
        })
    }
}

impl std::str::FromStr for ArtworkKind {
    type Err = UnknownArtworkSegment;

    fn from_str(segment: &str) -> Result<Self, Self::Err> {
        match segment {
            "movie" => Ok(Self::Movie),
            "show" => Ok(Self::Show),
            "season" => Ok(Self::Season),
            "episode" => Ok(Self::Episode),
            _ => Err(UnknownArtworkSegment),
        }
    }
}

impl std::fmt::Display for ArtworkVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Poster => "poster",
            Self::Backdrop => "backdrop",
            Self::Thumbnail => "thumbnail",
        })
    }
}

impl std::str::FromStr for ArtworkVariant {
    type Err = UnknownArtworkSegment;

    fn from_str(segment: &str) -> Result<Self, Self::Err> {
        match segment {
            "poster" => Ok(Self::Poster),
            "backdrop" => Ok(Self::Backdrop),
            "thumbnail" => Ok(Self::Thumbnail),
            _ => Err(UnknownArtworkSegment),
        }
    }
}

/// The path a client fetches one title's artwork from.
///
/// Relative, exactly as `stream_url` is: the client resolves it against the
/// origin it is already talking to, which is what lets one server be reached
/// over a LAN address, a domain and a reverse proxy without the stored URL
/// being wrong for two of them.
pub fn artwork_path(
    kind: ArtworkKind,
    id: impl std::fmt::Display,
    variant: ArtworkVariant,
) -> String {
    format!("/v1/artwork/{kind}/{id}/{variant}")
}
