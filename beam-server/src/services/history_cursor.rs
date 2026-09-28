//! The watch-history cursor (issue #188): an opaque token naming the row a
//! page ended on, by the list's order -- when it was last played, and its id
//! -- as base64url JSON:
//!
//! ```json
//! {"v":1,"played_at":"2026-09-28T12:00:00.123456Z","id":"…"}
//! ```
//!
//! As for the browse and enrichment cursors, clients treat it as opaque, and
//! it is not signed: it holds nothing the caller could not see, and every
//! read it seeks is scoped to the caller's own rows. Its member names are its
//! own, so a cursor another list issued is refused rather than read as a
//! position here.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::watch_state::HistoryPosition;

/// The cursor layout this build writes and reads.
const VERSION: u8 = 1;

/// Why a cursor was refused: every case is the caller's to fix.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("the cursor is not one the history list issued")]
    Malformed,
    #[error("the cursor was issued by a different version of the server")]
    WrongVersion,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBody {
    v: u8,
    played_at: String,
    id: Uuid,
}

/// The cursor for `position`.
#[must_use]
pub fn encode(position: &HistoryPosition) -> String {
    let body = CursorBody {
        v: VERSION,
        // Every digit the time has: a cursor rounded coarser than the row it
        // came from would seek past rows that share its second.
        played_at: position
            .last_played_at
            .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        id: position.id,
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The position `cursor` names.
pub fn decode(cursor: &str) -> Result<HistoryPosition, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let CursorBody { v, played_at, id } =
        serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    let last_played_at = DateTime::parse_from_rfc3339(&played_at)
        .map_err(|_| CursorError::Malformed)?
        .with_timezone(&Utc);
    Ok(HistoryPosition { last_played_at, id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every position round-trips exactly, to the nanosecond: a cursor
        /// that lost a digit would skip or repeat rows at a page boundary.
        #[test]
        fn a_position_round_trips(secs in 0i64..4_000_000_000, nanos in 0u32..1_000_000_000, id in any::<u128>()) {
            let position = HistoryPosition {
                last_played_at: DateTime::from_timestamp(secs, nanos).unwrap(),
                id: Uuid::from_u128(id),
            };
            prop_assert_eq!(decode(&encode(&position)), Ok(position));
        }

        #[test]
        fn decoding_never_panics(cursor in "\\PC{0,64}") {
            let _ = decode(&cursor);
        }
    }

    #[test]
    fn what_this_list_did_not_issue_is_refused() {
        let encode_json = |json: &str| URL_SAFE_NO_PAD.encode(json.as_bytes());
        let id = Uuid::nil();
        let enrichment = crate::services::enrichment_cursor::encode(
            &beam_domain::models::enrichment::EnrichmentListPosition {
                updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                id,
            },
        );
        for (cursor, why) in [
            ("not base64!".to_string(), "not base64"),
            (encode_json("[]"), "not the body"),
            (
                encode_json(&format!(r#"{{"v":1,"played_at":"yesterday","id":"{id}"}}"#)),
                "not a time",
            ),
            (enrichment, "the enrichment list's cursor"),
        ] {
            assert_eq!(decode(&cursor), Err(CursorError::Malformed), "{why}");
        }
        assert_eq!(
            decode(&encode_json(&format!(
                r#"{{"v":2,"played_at":"2026-01-01T00:00:00Z","id":"{id}"}}"#
            ))),
            Err(CursorError::WrongVersion)
        );
    }
}
