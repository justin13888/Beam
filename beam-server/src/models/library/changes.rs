//! `GET /v1/libraries/changes` (issue #454): which libraries the filesystem
//! watcher changed since a client last asked.

use beam_index::services::notification::LibraryChangeCursor;
use kynos::Schema;
use serde::Serialize;
use uuid::Uuid;

/// The libraries changed since the cursor a client passed: what
/// `GET /v1/libraries/changes` returns.
///
/// A client keeps `cursor` and passes it back as `since` on its next poll.
/// When `reset` is `true` the server cannot say what changed -- there was no
/// cursor, or it was issued before the server restarted -- and the client is
/// to refetch every library it shows; `library_ids` is then empty.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct LibraryChanges {
    /// Opaque. Pass it as `since` on the next poll.
    pub cursor: String,
    /// Whether every library is to be refetched, the cursor passed being
    /// unknown to this server.
    pub reset: bool,
    /// The libraries the caller can see whose contents changed since the
    /// cursor passed, each once, in no particular order.
    pub library_ids: Vec<Uuid>,
}

/// `cursor` as the opaque string a client holds: `"{epoch}.{sequence}"`.
#[must_use]
pub fn encode_change_cursor(cursor: LibraryChangeCursor) -> String {
    let LibraryChangeCursor { epoch, sequence } = cursor;
    format!("{epoch}.{sequence}")
}

/// The cursor `encoded` names, or `None` when it names none. A client holds
/// whatever it was given, so a malformed one is read as no cursor -- a reset
/// -- rather than refused: refusing would leave the client with no way back.
#[must_use]
pub fn decode_change_cursor(encoded: &str) -> Option<LibraryChangeCursor> {
    let (epoch, sequence) = encoded.split_once('.')?;
    Some(LibraryChangeCursor {
        epoch: Uuid::parse_str(epoch).ok()?,
        sequence: sequence.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every cursor the server issues reads back as itself.
        #[test]
        fn an_encoded_cursor_decodes_to_itself(epoch in any::<u128>(), sequence in any::<u64>()) {
            let cursor = LibraryChangeCursor { epoch: Uuid::from_u128(epoch), sequence };
            prop_assert_eq!(decode_change_cursor(&encode_change_cursor(cursor)), Some(cursor));
        }

        /// Whatever a client sends, decoding it never panics.
        #[test]
        fn decoding_any_string_never_panics(encoded in ".*") {
            let _ = decode_change_cursor(&encoded);
        }
    }

    #[test]
    fn a_malformed_cursor_names_none() {
        let epoch = Uuid::from_u128(7);
        for encoded in [
            String::new(),
            "not-a-cursor".to_owned(),
            format!("{epoch}"),
            format!("{epoch}."),
            format!("{epoch}.-1"),
            format!("{epoch}.1.2"),
            "nope.3".to_owned(),
        ] {
            assert_eq!(decode_change_cursor(&encoded), None, "{encoded:?}");
        }
    }
}
