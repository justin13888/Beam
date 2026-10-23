//! The catalogue read model: one ordered, filtered, paged listing of every
//! browsable title, movies and shows together (issue #187).
//!
//! Browse and search are one question -- "which titles, in what order, from
//! where" -- asked of two tables. Answering it per table and merging in memory
//! is what used to load the whole library for every page. These types state the
//! question once so the store can answer it with one ordered, limited query.
//!
//! A page boundary is a [`CatalogPosition`]: the sort key of a row plus its
//! `(id, kind)` tie-break. Positions are values, not offsets, so a page after
//! a position still starts in the right place when rows before it have been
//! added or removed since -- including the row the position was taken from.

use std::cmp::Ordering;
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Whether a catalogue entry is a film or a series.
///
/// Ordered `Movie` before `Show`, the order their lowercase names sort in, so
/// the in-memory ordering and the SQL `kind` text column agree on the last
/// tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TitleKind {
    Movie,
    Show,
}

impl TitleKind {
    /// The spelling the SQL read model projects and a cursor carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Show => "show",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "movie" => Some(Self::Movie),
            "show" => Some(Self::Show),
            _ => None,
        }
    }
}

/// What the catalogue can be ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogSortField {
    /// Display title, case-insensitively.
    Title,
    /// Release year; titles without one sort last.
    Year,
    /// Provider rating; unrated titles sort last.
    Rating,
    /// When the title was first indexed.
    DateAdded,
    /// Runtime in minutes; shows, and films without one, sort last.
    Runtime,
}

/// Which way a sort runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// A complete ordering: the field, its direction, and -- implicitly -- the
/// `(id, kind)` tie-break in the same direction, so no two titles are ever
/// equal and a page boundary is always exact.
///
/// The id breaks ties before the kind so that, within one kind's table, the
/// order is `(key, id)` -- the shape of the index a page is read through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CatalogSort {
    pub field: CatalogSortField,
    pub direction: SortDirection,
}

/// A title's value for one sort field.
///
/// The nullable fields carry `None` rather than a sentinel: a missing year is
/// not year zero, and it sorts after every present value in both directions.
#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    /// The lowercased display title.
    Title(String),
    Year(Option<i32>),
    /// On the provider's 0-10 scale.
    Rating(Option<f32>),
    DateAdded(DateTime<Utc>),
    /// Whole minutes.
    Runtime(Option<i32>),
}

impl SortKey {
    /// The field this key is a value of.
    #[must_use]
    pub const fn field(&self) -> CatalogSortField {
        match self {
            Self::Title(_) => CatalogSortField::Title,
            Self::Year(_) => CatalogSortField::Year,
            Self::Rating(_) => CatalogSortField::Rating,
            Self::DateAdded(_) => CatalogSortField::DateAdded,
            Self::Runtime(_) => CatalogSortField::Runtime,
        }
    }
}

/// One title's place in an ordering, and the boundary a page is taken from.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogPosition {
    pub kind: TitleKind,
    pub id: Uuid,
    pub key: SortKey,
}

impl CatalogPosition {
    /// Where `self` falls relative to `other` in `direction`'s display order:
    /// the key first, a missing value after every present one whichever way
    /// the sort runs, then `(id, kind)`.
    ///
    /// Keys of different fields never meet in one ordering; if they do, they
    /// compare by field so the result is still a total order.
    #[must_use]
    pub fn display_cmp(&self, other: &Self, direction: SortDirection) -> Ordering {
        fn nullable<T>(
            a: Option<T>,
            b: Option<T>,
            present: impl FnOnce(T, T) -> Ordering,
        ) -> Ordering {
            match (a, b) {
                (Some(a), Some(b)) => present(a, b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        let directed = |ordering: Ordering| match direction {
            SortDirection::Asc => ordering,
            SortDirection::Desc => ordering.reverse(),
        };

        let key = match (&self.key, &other.key) {
            (SortKey::Title(a), SortKey::Title(b)) => directed(a.cmp(b)),
            (SortKey::DateAdded(a), SortKey::DateAdded(b)) => directed(a.cmp(b)),
            (SortKey::Year(a), SortKey::Year(b)) | (SortKey::Runtime(a), SortKey::Runtime(b)) => {
                nullable(*a, *b, |a, b| directed(a.cmp(&b)))
            }
            (SortKey::Rating(a), SortKey::Rating(b)) => {
                nullable(*a, *b, |a, b| directed(a.total_cmp(&b)))
            }
            (a, b) => (a.field() as u8).cmp(&(b.field() as u8)),
        };
        key.then_with(|| directed((self.id, self.kind).cmp(&(other.id, other.kind))))
    }
}

/// Which titles are listed. Every filter narrows; none is set by default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogFilters {
    /// Only films, or only series.
    pub kind: Option<TitleKind>,
    /// Titles resembling this text (trigram similarity, or containing it).
    pub query: Option<String>,
    /// Titles carrying the genre with this slug
    /// (see [`crate::repositories::genre::slugify`]).
    pub genre_slug: Option<String>,
    /// Signed, as the `year` columns are: a bound the column cannot hold is
    /// refused where the request is read, never wrapped into one it can.
    pub year: Option<i32>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Minimum rating on the provider's 0-10 scale, in the single precision
    /// the rating is stored in; an unrated title counts as 0.
    pub min_rating: Option<f32>,
}

/// Which side of a position a page is taken from.
#[derive(Debug, Clone, PartialEq)]
pub enum Seek {
    /// The first rows after the position, or from the start.
    Forward(Option<CatalogPosition>),
    /// The last rows before the position, or before the end. Still returned
    /// in display order.
    Backward(Option<CatalogPosition>),
}

/// One page request against the catalogue.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogQuery {
    pub filters: CatalogFilters,
    pub sort: CatalogSort,
    pub seek: Seek,
    /// At most this many rows. Never zero: an empty page is asked for by not
    /// asking.
    pub limit: NonZeroU32,
}

impl CatalogQuery {
    /// The position this query seeks from, if any.
    #[must_use]
    pub fn position(&self) -> Option<&CatalogPosition> {
        match &self.seek {
            Seek::Forward(position) | Seek::Backward(position) => position.as_ref(),
        }
    }
}

/// How many seasons and episodes a show has, for a browse tile that does not
/// load them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShowChildCounts {
    pub seasons: u32,
    pub episodes: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(kind: TitleKind, id: u128, key: SortKey) -> CatalogPosition {
        CatalogPosition {
            kind,
            id: Uuid::from_u128(id),
            key,
        }
    }

    /// A missing value sorts after every present one in both directions;
    /// present values follow the direction; ties fall to `(id, kind)` in the
    /// direction too.
    #[test]
    fn display_order_puts_missing_values_last_either_way() {
        let cases = [
            (SortDirection::Asc, Some(1), Some(2), Ordering::Less),
            (SortDirection::Desc, Some(1), Some(2), Ordering::Greater),
            (SortDirection::Asc, None, Some(2), Ordering::Greater),
            (SortDirection::Desc, None, Some(2), Ordering::Greater),
            (SortDirection::Asc, Some(2), None, Ordering::Less),
            (SortDirection::Desc, Some(2), None, Ordering::Less),
        ];
        for (direction, a, b, expected) in cases {
            let left = at(TitleKind::Movie, 1, SortKey::Year(a));
            let right = at(TitleKind::Movie, 2, SortKey::Year(b));
            assert_eq!(
                left.display_cmp(&right, direction),
                expected,
                "{direction:?} {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn equal_keys_fall_to_id_then_kind_in_the_sort_direction() {
        // The id decides before the kind: a show with the lower id sorts
        // first ascending although `Movie` sorts before `Show`.
        let movie = at(TitleKind::Movie, 9, SortKey::Year(None));
        let show = at(TitleKind::Show, 1, SortKey::Year(None));
        assert_eq!(show.display_cmp(&movie, SortDirection::Asc), Ordering::Less);
        assert_eq!(
            show.display_cmp(&movie, SortDirection::Desc),
            Ordering::Greater
        );

        // Only a shared id falls to the kind, in the direction too.
        let movie = at(TitleKind::Movie, 1, SortKey::Title("same".into()));
        let show = at(TitleKind::Show, 1, SortKey::Title("same".into()));
        assert_eq!(movie.display_cmp(&show, SortDirection::Asc), Ordering::Less);
        assert_eq!(
            movie.display_cmp(&show, SortDirection::Desc),
            Ordering::Greater
        );
    }

    #[test]
    fn kinds_round_trip_through_their_spelling() {
        for kind in [TitleKind::Movie, TitleKind::Show] {
            assert_eq!(TitleKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TitleKind::parse("episode"), None);
    }
}
