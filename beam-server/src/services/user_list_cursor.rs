//! The watchlist and favourites cursor (issue #262): an opaque token naming
//! the list it was issued by and the entry a page ended on, by the list's
//! order -- when it was added, and its id -- as base64url JSON:
//!
//! ```json
//! {"v":1,"list":"watchlist","added_at":"2026-10-27T12:00:00.123456Z","id":"…"}
//! ```
//!
//! As for the history cursor, clients treat it as opaque, and it is not
//! signed: it holds nothing the caller could not see, and every read it seeks
//! is scoped to the caller's own entries. It names its list, so the
//! watchlist's cursor is refused by favourites, and so is any other list's.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::user_list::{UserList, UserListPosition};

/// The cursor layout this build writes and reads.
const VERSION: u8 = 1;

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
    list: String,
    added_at: String,
    id: Uuid,
}

/// The cursor for `position` in `list`.
#[must_use]
pub fn encode(list: UserList, position: &UserListPosition) -> String {
    let body = CursorBody {
        v: VERSION,
        list: list.as_str().to_string(),
        // Every digit the time has: a cursor rounded coarser than the entry
        // it came from would seek past entries that share its second.
        added_at: position
            .added_at
            .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        id: position.id,
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The position `cursor` names in `list`.
pub fn decode(list: UserList, cursor: &str) -> Result<UserListPosition, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let CursorBody {
        v,
        list: issuer,
        added_at,
        id,
    } = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    if issuer != list.as_str() {
        return Err(CursorError::Malformed);
    }
    let added_at = DateTime::parse_from_rfc3339(&added_at)
        .map_err(|_| CursorError::Malformed)?
        .with_timezone(&Utc);
    Ok(UserListPosition { added_at, id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every position round-trips exactly, to the nanosecond, in each
        /// list: a cursor that lost a digit would skip or repeat entries at
        /// a page boundary.
        #[test]
        fn a_position_round_trips(secs in 0i64..4_000_000_000, nanos in 0u32..1_000_000_000, id in any::<u128>()) {
            let position = UserListPosition {
                added_at: DateTime::from_timestamp(secs, nanos).unwrap(),
                id: Uuid::from_u128(id),
            };
            for list in UserList::ALL {
                prop_assert_eq!(decode(list, &encode(list, &position)), Ok(position));
            }
        }

        #[test]
        fn decoding_never_panics(cursor in "\\PC{0,64}") {
            let _ = decode(UserList::Watchlist, &cursor);
        }
    }

    #[test]
    fn what_this_list_did_not_issue_is_refused() {
        let encode_json = |json: &str| URL_SAFE_NO_PAD.encode(json.as_bytes());
        let id = Uuid::nil();
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let history = crate::services::history_cursor::encode(
            &beam_domain::models::watch_state::HistoryPosition {
                last_played_at: at,
                id,
            },
        );
        let favourites = encode(UserList::Favourites, &UserListPosition { added_at: at, id });
        for (cursor, why) in [
            ("not base64!".to_string(), "not base64"),
            (encode_json("[]"), "not the body"),
            (
                encode_json(&format!(
                    r#"{{"v":1,"list":"watchlist","added_at":"yesterday","id":"{id}"}}"#
                )),
                "not a time",
            ),
            (favourites, "the favourites list's cursor"),
            (history, "the history list's cursor"),
        ] {
            assert_eq!(
                decode(UserList::Watchlist, &cursor),
                Err(CursorError::Malformed),
                "{why}"
            );
        }
        assert_eq!(
            decode(
                UserList::Watchlist,
                &encode_json(&format!(
                    r#"{{"v":2,"list":"watchlist","added_at":"2026-01-01T00:00:00Z","id":"{id}"}}"#
                ))
            ),
            Err(CursorError::WrongVersion)
        );
    }
}
