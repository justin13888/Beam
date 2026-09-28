//! The admin enrichment list's cursor (issue #185): an opaque token naming
//! the row a page ended on, by the list's order -- when it last changed, and
//! its id -- as base64url JSON:
//!
//! ```json
//! {"v":1,"at":"2026-09-28T12:00:00.123456Z","id":"…"}
//! ```
//!
//! As for the browse cursor, clients treat it as opaque, and it is not
//! signed: it holds nothing a caller could not see, and a forged one only
//! seeks to a place in a list the caller could page to anyway. The filters
//! are not in it: the list's order is the same under every filter, so a
//! position is one whatever the filters are.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::enrichment::EnrichmentListPosition;

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
    at: String,
    id: Uuid,
}

/// The cursor for `position`.
#[must_use]
pub fn encode(position: &EnrichmentListPosition) -> String {
    let body = CursorBody {
        v: VERSION,
        // Every digit the time has: a cursor rounded coarser than the row it
        // came from would seek past rows that share its second.
        at: position
            .updated_at
            .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        id: position.id,
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The position `cursor` names.
pub fn decode(cursor: &str) -> Result<EnrichmentListPosition, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let CursorBody { v, at, id } =
        serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    let updated_at = DateTime::parse_from_rfc3339(&at)
        .map_err(|_| CursorError::Malformed)?
        .with_timezone(&Utc);
    Ok(EnrichmentListPosition { updated_at, id })
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
            let position = EnrichmentListPosition {
                updated_at: DateTime::from_timestamp(secs, nanos).unwrap(),
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
        for (cursor, why) in [
            ("not base64!".to_string(), "not base64"),
            (encode_json("[]"), "not the body"),
            (
                encode_json(&format!(r#"{{"v":1,"at":"yesterday","id":"{id}"}}"#)),
                "not a time",
            ),
            (
                encode_json(&format!(
                    r#"{{"v":1,"at":"2026-01-01T00:00:00Z","id":"{id}","sort":"x"}}"#
                )),
                "a member the body does not have",
            ),
        ] {
            assert_eq!(decode(&cursor), Err(CursorError::Malformed), "{why}");
        }
        assert_eq!(
            decode(&encode_json(&format!(
                r#"{{"v":2,"at":"2026-01-01T00:00:00Z","id":"{id}"}}"#
            ))),
            Err(CursorError::WrongVersion)
        );
    }
}
