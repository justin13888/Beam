//! The cursor of a library's file list (issue #190): an opaque token naming
//! the file a page ended on, by the list's order -- its path, then its id --
//! and the library it belongs to, as base64url JSON:
//!
//! ```json
//! {"v":1,"library_id":"…","path":"Movies/Heat (1995).mkv","id":"…"}
//! ```
//!
//! The path is the one the page reported: relative to the library's root,
//! never the absolute path the file is stored under, which is not a client's
//! to see (NFR-108). It is not signed: it holds nothing the caller could not
//! see, and a forged one only seeks to a place in a list they could page to
//! anyway -- the path is a position compared against, never opened. A cursor
//! names its library, so one from another library's list, or from any other
//! list, is refused rather than read as a position here.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The cursor layout this build writes and reads.
const VERSION: u8 = 1;

/// Why a cursor was refused: every case is the caller's to fix.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("the cursor is not one this library's file list issued")]
    Malformed,
    #[error("the cursor was issued by a different version of the server")]
    WrongVersion,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBody {
    v: u8,
    library_id: Uuid,
    path: String,
    id: Uuid,
}

/// The cursor naming the file `id` at `path`, relative to the root of
/// `library_id`.
#[must_use]
pub fn encode(library_id: Uuid, path: &str, id: Uuid) -> String {
    let body = CursorBody {
        v: VERSION,
        library_id,
        path: path.to_owned(),
        id,
    };
    let json = serde_json::to_vec(&body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The relative path and id of the file `cursor` names in `library_id`'s
/// list.
pub fn decode(library_id: Uuid, cursor: &str) -> Result<(String, Uuid), CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| CursorError::Malformed)?;
    let CursorBody {
        v,
        library_id: issuer,
        path,
        id,
    } = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
    if v != VERSION {
        return Err(CursorError::WrongVersion);
    }
    if issuer != library_id {
        return Err(CursorError::Malformed);
    }
    Ok((path, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every position round-trips exactly: a cursor that changed a path
        /// would seek past, or short of, the file it names.
        #[test]
        fn a_position_round_trips(path in "\\PC{0,64}", library in any::<u128>(), id in any::<u128>()) {
            let library = Uuid::from_u128(library);
            let id = Uuid::from_u128(id);
            prop_assert_eq!(decode(library, &encode(library, &path, id)), Ok((path, id)));
        }

        #[test]
        fn decoding_never_panics(cursor in "\\PC{0,64}") {
            let _ = decode(Uuid::nil(), &cursor);
        }
    }

    #[test]
    fn what_this_list_did_not_issue_is_refused() {
        let encode_json = |json: &str| URL_SAFE_NO_PAD.encode(json.as_bytes());
        let library = Uuid::from_u128(1);
        let other_library = Uuid::from_u128(2);
        let id = Uuid::nil();
        let users = crate::services::offset_cursor::encode(
            crate::services::offset_cursor::OffsetList::AdminUsers,
            0,
        );
        for (cursor, why) in [
            ("not base64!".to_owned(), "not base64"),
            (encode_json("[]"), "not the body"),
            (
                encode(other_library, "a.mkv", id),
                "another library's cursor",
            ),
            (users, "the admin user list's cursor"),
        ] {
            assert_eq!(
                decode(library, &cursor),
                Err(CursorError::Malformed),
                "{why}"
            );
        }
        assert_eq!(
            decode(
                library,
                &encode_json(&format!(
                    r#"{{"v":2,"library_id":"{library}","path":"a.mkv","id":"{id}"}}"#
                ))
            ),
            Err(CursorError::WrongVersion)
        );
    }
}
