//! How artwork is addressed.
//!
//! Shared by the endpoint that serves it and the metadata service that points
//! clients at it: one spelling of `movie`/`poster` rather than two that drift.

use kynos::Schema;
use serde::{Deserialize, Serialize};

/// What the artwork belongs to: a kind of title, a person, or a collection.
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
    /// Someone credited on a title (issue #261).
    Person,
    /// A provider collection of movies (issue #262).
    Collection,
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
    /// A person's portrait.
    Profile,
}

/// How wide an image a client wants (issue #266): a fixed ladder of widths, in
/// pixels.
///
/// A ladder rather than any width a client names, because every size is a file
/// in the cache: an open integer would let one client fill
/// `BEAM_ARTWORK_CACHE_MAX_BYTES` with a thousand renditions of one poster. The
/// rungs match the widths TMDB serves, which is where the art usually comes
/// from, so a client that already knows those sizes picks the same ones here. A
/// client picks the smallest rung at least as wide as the pixels it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkSize {
    /// 185 pixels wide: a phone's poster grid.
    W185,
    /// 342 pixels wide: a tablet's or a dense desktop grid.
    W342,
    /// 500 pixels wide: a detail page's poster.
    W500,
    /// 780 pixels wide: a phone's backdrop.
    W780,
    /// 1280 pixels wide: a desktop or TV backdrop.
    W1280,
}

impl ArtworkSize {
    /// Every rung, narrowest first.
    pub const ALL: &'static [Self] = &[Self::W185, Self::W342, Self::W500, Self::W780, Self::W1280];

    /// The width this rung scales an image to, in pixels.
    pub fn width(self) -> std::num::NonZeroU32 {
        let width = match self {
            Self::W185 => 185,
            Self::W342 => 342,
            Self::W500 => 500,
            Self::W780 => 780,
            Self::W1280 => 1280,
        };
        std::num::NonZeroU32::new(width).expect("every rung is a positive width")
    }
}

impl std::fmt::Display for ArtworkSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "w{}", self.width())
    }
}

impl std::str::FromStr for ArtworkSize {
    type Err = UnknownArtworkSegment;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|size| size.to_string() == value)
            .ok_or(UnknownArtworkSegment)
    }
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
            Self::Person => "person",
            Self::Collection => "collection",
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
            "person" => Ok(Self::Person),
            "collection" => Ok(Self::Collection),
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
            Self::Profile => "profile",
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
            "profile" => Ok(Self::Profile),
            _ => Err(UnknownArtworkSegment),
        }
    }
}

// Both are path segments of `/v1/artwork/{kind}/{id}/{variant}`, so both are
// one string value on the wire. Kynos asks for the opt-in because it cannot see
// that from the schema alone.
impl kynos::schema::ParamValue for ArtworkKind {}
impl kynos::schema::ParamValue for ArtworkVariant {}
// The `size` query parameter, likewise one string value.
impl kynos::schema::ParamValue for ArtworkSize {}

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

#[cfg(test)]
mod tests {
    use super::ArtworkSize;

    /// The description lists the serde spelling and the route parses the
    /// `FromStr` one, so the two must be one spelling -- and the ladder must
    /// climb, or "the smallest rung at least this wide" means nothing.
    #[test]
    fn every_size_parses_from_the_spelling_it_is_described_with() {
        for size in ArtworkSize::ALL {
            let described = serde_json::to_value(size).expect("serialises");
            let spelled = described.as_str().expect("a string value");
            assert_eq!(spelled.parse::<ArtworkSize>().ok(), Some(*size));
            assert_eq!(size.to_string(), spelled);
        }
        assert!(
            ArtworkSize::ALL
                .windows(2)
                .all(|pair| pair[0].width() < pair[1].width())
        );
    }
}
