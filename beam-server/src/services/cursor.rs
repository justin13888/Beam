//! The browse cursor: an opaque token naming a page boundary (issue #187).
//!
//! A cursor is a [`CatalogPosition`] -- the sort key of the title a page ended
//! (or started) on, and that title's `(kind, id)` -- plus the sort it was
//! taken under, as base64url JSON:
//!
//! ```json
//! {"v":1,"sort":"year","order":"desc","kind":"movie","id":"…","key":1999}
//! ```
//!
//! Clients must treat it as opaque; the shape is documented here so the
//! server can change it behind the version. It is **not** signed: everything
//! in it is either public (a title's id and its sort value) or checked
//! against the request (the sort), and a forged position only ever seeks to a
//! place in a listing the caller could page to anyway.
//!
//! The key is kept, not just the id, so a page after a title that has since
//! gone -- its file removed, its title renamed -- still starts in the right
//! place. A raw id could only restart from the beginning.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::catalog::{CatalogPosition, SortKey, TitleKind};

use crate::models::search::{MediaSortField, SortOrder};

/// The cursor layout this build writes and reads.
const VERSION: u8 = 1;

/// Why a cursor was refused. Every case is the caller's to fix, so each maps
/// to one 400.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("the cursor is not one this server issued")]
    Malformed,
    #[error("the cursor was issued by a different version of the server")]
    WrongVersion,
    #[error(
        "the cursor was issued for sort_by={sort}&sort_order={order}; pass the same sort to page on"
    )]
    SortMismatch { sort: String, order: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBody {
    v: u8,
    sort: String,
    order: String,
    kind: String,
    id: Uuid,
    key: serde_json::Value,
}

fn key_value(key: &SortKey) -> serde_json::Value {
    match key {
        SortKey::Title(title) => serde_json::Value::from(title.as_str()),
        SortKey::Year(year) | SortKey::Runtime(year) => serde_json::Value::from(*year),
        // Through `f64`, which holds every `f32` exactly; `from_value` below
        // narrows it back without loss.
        SortKey::Rating(rating) => serde_json::Value::from(rating.map(f64::from)),
        SortKey::DateAdded(at) => serde_json::Value::from(at.to_rfc3339()),
    }
}

fn key_from(sort: MediaSortField, value: serde_json::Value) -> Option<SortKey> {
    fn nullable<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Option<Option<T>> {
        serde_json::from_value(value).ok()
    }
    Some(match sort {
        MediaSortField::Title => SortKey::Title(value.as_str()?.to_owned()),
        MediaSortField::Year => SortKey::Year(nullable(value)?),
        MediaSortField::Runtime => SortKey::Runtime(nullable(value)?),
        MediaSortField::Rating => {
            let rating: Option<f64> = nullable(value)?;
            let narrowed = rating.map(|r| r as f32);
            // A value no `f32` holds was not written by `encode`.
            if rating
                .zip(narrowed)
                .is_some_and(|(wide, narrow)| f64::from(narrow) != wide)
            {
                return None;
            }
            SortKey::Rating(narrowed)
        }
        MediaSortField::DateAdded => SortKey::DateAdded(
            DateTime::parse_from_rfc3339(value.as_str()?)
                .ok()?
                .with_timezone(&Utc),
        ),
    })
}

/// The cursor for `position` in a listing sorted by `sort` and `order`.
#[must_use]
pub fn encode(sort: MediaSortField, order: SortOrder, position: &CatalogPosition) -> String {
    let body = CursorBody {
        v: VERSION,
        sort: sort.as_str().to_owned(),
        order: order.as_str().to_owned(),
        kind: position.kind.as_str().to_owned(),
        id: position.id,
        key: key_value(&position.key),
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The position `cursor` names, provided it was issued for `sort` and `order`.
pub fn decode(
    cursor: &str,
    sort: MediaSortField,
    order: SortOrder,
) -> Result<CatalogPosition, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let body: CursorBody = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    let CursorBody {
        v,
        sort: cursor_sort,
        order: cursor_order,
        kind,
        id,
        key,
    } = body;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    if cursor_sort != sort.as_str() || cursor_order != order.as_str() {
        return Err(CursorError::SortMismatch {
            sort: cursor_sort,
            order: cursor_order,
        });
    }
    let kind = TitleKind::parse(&kind).ok_or(CursorError::Malformed)?;
    let key = key_from(sort, key).ok_or(CursorError::Malformed)?;
    Ok(CatalogPosition { kind, id, key })
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
