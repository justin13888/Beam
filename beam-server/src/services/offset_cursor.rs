//! The cursor of a list paged by offset (issue #190): the admin user list,
//! the admin log, people search (issue #261), and the collection listing
//! (issue #262). An opaque token naming the list it was issued by and the
//! position of the row a page ended on, as base64url JSON:
//!
//! ```json
//! {"v":1,"list":"admin_users","at":49}
//! ```
//!
//! Each list is read by `LIMIT`/`OFFSET` underneath, so the position is an
//! offset; putting it inside a cursor is what lets the wire speak
//! `first`/`after` like every other `/v1` list, and lets any of them move to
//! a keyset later without a client noticing. Like the other cursors it is not
//! signed: it holds nothing its caller could not see, and a forged one only
//! seeks to a place they could page to anyway. A cursor names its list, so
//! one issued by another list -- an offset-paged one or any other, whose
//! members are not these -- is refused rather than read as a position here. So is a
//! position past [`MAX_AT`]: no list holds that many rows, and the page after
//! it would start beyond the `bigint` Postgres reads `OFFSET` as, turning a
//! forged cursor into a database error rather than a `400`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// The cursor layout this build writes and reads.
const VERSION: u8 = 1;

/// The furthest row a cursor can name: the page after it starts at
/// `i64::MAX`, the largest `OFFSET` Postgres accepts.
pub const MAX_AT: u64 = i64::MAX.unsigned_abs() - 1;

/// The lists an offset cursor can belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffsetList {
    /// `GET /v1/admin/users`.
    AdminUsers,
    /// `GET /v1/admin/logs`.
    AdminLogs,
    /// `GET /v1/people`.
    People,
    /// `GET /v1/collections` (issue #262).
    Collections,
}

/// Why a cursor was refused: every case is the caller's to fix.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("the cursor is not one this list issued")]
    Malformed,
    #[error("the cursor was issued by a different version of the server")]
    WrongVersion,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBody {
    v: u8,
    list: OffsetList,
    at: u64,
}

/// The cursor naming the row at offset `at` of `list`.
#[must_use]
pub fn encode(list: OffsetList, at: u64) -> String {
    let body = CursorBody {
        v: VERSION,
        list,
        at,
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The offset of the row `cursor` names in `list`.
pub fn decode(list: OffsetList, cursor: &str) -> Result<u64, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let CursorBody {
        v,
        list: issuer,
        at,
    } = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    if issuer != list || at > MAX_AT {
        return Err(CursorError::Malformed);
    }
    Ok(at)
}

/// Where a page of an offset-paged list starts: the row after the one `after`
/// names, or the first row.
///
/// # Errors
///
/// As [`decode`].
pub fn start_after(list: OffsetList, after: Option<&str>) -> Result<u64, CursorError> {
    match after {
        Some(cursor) => Ok(decode(list, cursor)? + 1),
        None => Ok(0),
    }
}

/// The `page_info` of a page of `len` rows starting at `offset`, out of
/// `total`.
#[must_use]
pub fn page_info(
    list: OffsetList,
    offset: u64,
    len: usize,
    total: u64,
) -> crate::models::search::PageInfo {
    let len = len as u64;
    let (start_cursor, end_cursor) = if len == 0 {
        (None, None)
    } else {
        (
            Some(encode(list, offset)),
            Some(encode(list, offset + len - 1)),
        )
    };
    crate::models::search::PageInfo {
        has_next_page: offset.saturating_add(len) < total,
        has_previous_page: offset > 0,
        start_cursor,
        end_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every offset round-trips, and the page after a cursor starts on the
        /// row after the one it names: a cursor that lost or gained a row
        /// would skip or repeat it at a page boundary.
        #[test]
        fn an_offset_round_trips(at in 0u64..=MAX_AT) {
            for list in [
                OffsetList::AdminUsers,
                OffsetList::AdminLogs,
                OffsetList::People,
                OffsetList::Collections,
            ] {
                let cursor = encode(list, at);
                prop_assert_eq!(decode(list, &cursor), Ok(at));
                prop_assert_eq!(start_after(list, Some(&cursor)), Ok(at + 1));
            }
        }

        #[test]
        fn decoding_never_panics(cursor in "\\PC{0,64}") {
            let _ = decode(OffsetList::AdminLogs, &cursor);
        }

        /// The cursors bound the page exactly, and the flags agree with
        /// where it sits in the whole list.
        #[test]
        fn page_info_bounds_the_page(offset in 0u64..1000, len in 0usize..100, extra in 0u64..100) {
            let total = offset + len as u64 + extra;
            let info = page_info(OffsetList::AdminUsers, offset, len, total);
            prop_assert_eq!(info.has_previous_page, offset > 0);
            prop_assert_eq!(info.has_next_page, extra > 0);
            if len == 0 {
                prop_assert!(info.start_cursor.is_none() && info.end_cursor.is_none());
            } else {
                let start = decode(OffsetList::AdminUsers, info.start_cursor.as_deref().unwrap());
                let end = decode(OffsetList::AdminUsers, info.end_cursor.as_deref().unwrap());
                prop_assert_eq!(start, Ok(offset));
                prop_assert_eq!(end, Ok(offset + len as u64 - 1));
            }
        }
    }

    #[test]
    fn no_cursor_starts_at_the_first_row() {
        assert_eq!(start_after(OffsetList::AdminUsers, None), Ok(0));
    }

    #[test]
    fn what_this_list_did_not_issue_is_refused() {
        let encode_json = |json: &str| URL_SAFE_NO_PAD.encode(json.as_bytes());
        let history = crate::services::history_cursor::encode(
            &beam_domain::models::watch_state::HistoryPosition {
                last_played_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                id: uuid::Uuid::nil(),
            },
        );
        for (cursor, why) in [
            ("not base64!".to_owned(), "not base64"),
            (encode_json("[]"), "not the body"),
            (
                encode_json(r#"{"v":1,"list":"admin_users","at":-1}"#),
                "a negative offset",
            ),
            (
                encode_json(r#"{"v":1,"list":"genres","at":0}"#),
                "an unknown list",
            ),
            (encode(OffsetList::AdminLogs, 3), "the admin log's cursor"),
            (encode(OffsetList::People, 3), "people search's cursor"),
            (
                encode(OffsetList::Collections, 3),
                "the collection listing's cursor",
            ),
            (
                encode(OffsetList::AdminUsers, MAX_AT + 1),
                "a position whose next page is past Postgres's OFFSET",
            ),
            (encode(OffsetList::AdminUsers, u64::MAX), "the largest u64"),
            (history, "the history list's cursor"),
        ] {
            assert_eq!(
                decode(OffsetList::AdminUsers, &cursor),
                Err(CursorError::Malformed),
                "{why}"
            );
        }
        assert_eq!(
            decode(
                OffsetList::AdminUsers,
                &encode_json(r#"{"v":2,"list":"admin_users","at":0}"#)
            ),
            Err(CursorError::WrongVersion)
        );
    }
}
