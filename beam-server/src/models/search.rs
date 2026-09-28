//! Wire DTOs for media discovery: the typed sort/filter parameters and the
//! connection `GET /v1/media` returns.
//!
//! These lived in `services::metadata` while `salvo::oapi::ToSchema` was the
//! derive. ADR-0010 requires the service layer to stay transport-independent,
//! and a `Schema` derive is transport: the contract is what these types are
//! *for*. The service still owns the search itself and imports these from here.

use kynos::Schema;
use serde::{Deserialize, Serialize};

use crate::models::media::MediaMetadata;

/// Sort field options for media search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum MediaSortField {
    /// Sort by title (alphabetical)
    #[default]
    Title,
    /// Sort by release year
    Year,
    /// Sort by rating
    Rating,
    /// Sort by date added to library
    DateAdded,
    /// Sort by runtime/duration
    Runtime,
}

/// Sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Ascending order
    #[default]
    Asc,
    /// Descending order
    Desc,
}

/// Which kind of title something is: a movie, or a show. The one enum for it on
/// the wire -- the `media_type` browse filter, a continue-watching or history
/// item's `media_type`, and an enrichment row's `kind` all use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum TitleKind {
    /// A movie.
    Movie,
    /// A TV show.
    Show,
}

/// One page of browse or search results.
///
/// `items` are in the requested order. `page_info`'s cursors are opaque: pass
/// `end_cursor` as `after` for the next page, `start_cursor` as `before` for
/// the previous one, with the same `sort_by` and `sort_order`.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct MediaConnection {
    /// The titles on this page.
    pub items: Vec<MediaMetadata>,
    /// Where this page sits in the whole listing.
    pub page_info: PageInfo,
}

/// Where a page sits in the whole listing.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PageInfo {
    /// Whether a page follows this one.
    pub has_next_page: bool,
    /// Whether a page precedes this one.
    pub has_previous_page: bool,
    /// Cursor of the first item; absent on an empty page.
    pub start_cursor: Option<String>,
    /// Cursor of the last item; absent on an empty page.
    pub end_cursor: Option<String>,
}

// ── Query-parameter parsing ──────────────────────────────────────────────────
//
// Kynos reads a query parameter through `FromStr` rather than serde: a query
// string is text, and going through a JSON deserializer to read `sort_by=year`
// would mean the parameter's accepted values and its documented schema came
// from two different traits. These three spell the same wire form the
// `#[serde(rename_all = "snake_case")]` above produces, so the value a client
// sends and the value the document lists stay one vocabulary.

/// What a caller sent that none of the variants name.
#[derive(Debug, thiserror::Error)]
#[error("expected one of {expected}, got {got:?}")]
pub struct UnknownVariant {
    expected: &'static str,
    got: String,
}

impl UnknownVariant {
    /// `got` named none of the `expected` values.
    #[must_use]
    pub fn new(expected: &'static str, got: &str) -> Self {
        Self {
            expected,
            got: got.to_owned(),
        }
    }
}

/// The wire spelling of each variant, in one place.
///
/// `Display` and `FromStr` are the two halves of the same table, so they are
/// written against one `as_str` rather than as two independent `match`es that
/// could disagree about `date_added`.
impl MediaSortField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Year => "year",
            Self::Rating => "rating",
            Self::DateAdded => "date_added",
            Self::Runtime => "runtime",
        }
    }
}

impl SortOrder {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

impl TitleKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Show => "show",
        }
    }
}

impl std::fmt::Display for MediaSortField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for SortOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for TitleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MediaSortField {
    type Err = UnknownVariant;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "title" => Ok(Self::Title),
            "year" => Ok(Self::Year),
            "rating" => Ok(Self::Rating),
            "date_added" => Ok(Self::DateAdded),
            "runtime" => Ok(Self::Runtime),
            other => Err(UnknownVariant {
                expected: "title, year, rating, date_added, runtime",
                got: other.to_owned(),
            }),
        }
    }
}

impl std::str::FromStr for SortOrder {
    type Err = UnknownVariant;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            other => Err(UnknownVariant {
                expected: "asc, desc",
                got: other.to_owned(),
            }),
        }
    }
}

impl std::str::FromStr for TitleKind {
    type Err = UnknownVariant;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "movie" => Ok(Self::Movie),
            "show" => Ok(Self::Show),
            other => Err(UnknownVariant {
                expected: "movie, show",
                got: other.to_owned(),
            }),
        }
    }
}

// Each is read from a query parameter, so each is one string value on the wire.
// Kynos asks for the opt-in because it cannot see that from the schema alone.
impl kynos::schema::ParamValue for MediaSortField {}
impl kynos::schema::ParamValue for SortOrder {}
impl kynos::schema::ParamValue for TitleKind {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// The invariant that matters: every variant the document lists is one
    /// `FromStr` accepts, spelled identically. The expected spelling is taken
    /// from serde rather than restated here, so adding a variant without
    /// teaching `FromStr` about it fails this test instead of silently
    /// rejecting a documented value at run time.
    #[test]
    fn every_documented_sort_field_parses() {
        for field in [
            MediaSortField::Title,
            MediaSortField::Year,
            MediaSortField::Rating,
            MediaSortField::DateAdded,
            MediaSortField::Runtime,
        ] {
            let json = serde_json::to_string(&field).expect("serializes");
            let wire = json.trim_matches('"');
            assert_eq!(
                MediaSortField::from_str(wire).expect("round-trips"),
                field,
                "{wire} did not round-trip"
            );
            // `Display` is what builds a query string a client sends back,
            // so it has to agree with serde too -- otherwise a value read
            // out of the document could not be written into a request.
            assert_eq!(field.to_string(), wire);
        }
    }

    #[test]
    fn every_documented_sort_order_parses() {
        for order in [SortOrder::Asc, SortOrder::Desc] {
            let json = serde_json::to_string(&order).expect("serializes");
            let wire = json.trim_matches('"');
            assert_eq!(SortOrder::from_str(wire).expect("round-trips"), order);
            assert_eq!(order.to_string(), wire);
        }
    }

    #[test]
    fn every_documented_media_type_parses() {
        for kind in [TitleKind::Movie, TitleKind::Show] {
            let json = serde_json::to_string(&kind).expect("serializes");
            let wire = json.trim_matches('"');
            assert_eq!(TitleKind::from_str(wire).expect("round-trips"), kind);
            assert_eq!(kind.to_string(), wire);
        }
    }

    #[test]
    fn an_unknown_variant_is_rejected_rather_than_defaulted() {
        assert!(MediaSortField::from_str("popularity").is_err());
        assert!(SortOrder::from_str("ascending").is_err());
        assert!(TitleKind::from_str("episode").is_err());
    }
}
