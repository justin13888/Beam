//! The opaque page cursors every `/v1` list speaks `first`/`after` with.
//!
//! Each cursor is a small JSON body, opening with its layout version `v`, sent
//! as base64url without padding. The envelope -- [`seal`] and [`open`] -- is
//! shared here; each list's body, the checks it makes on what it opens, and
//! the error it answers with live in its own submodule, so a cursor issued by
//! one list is refused, with that list's message, by every other:
//!
//! - [`browse`]: the catalog browse and search listing (issue #187).
//! - [`enrichment`]: the admin enrichment list (issue #185).
//! - [`history`]: the watch history (issue #188).
//! - [`library_files`]: a library's file list (issue #190).
//! - [`offset`]: the lists paged by offset (issue #190).
//! - [`user_list`]: the watchlist and favourites (issue #262).
//!
//! None is signed: each holds nothing its caller could not see, and a forged
//! one only seeks to a place its caller could page to anyway.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod browse;
pub mod enrichment;
pub mod history;
pub mod library_files;
pub mod offset;
pub mod user_list;

/// The cursor layout this build writes and reads, as every body's `v`.
const VERSION: u8 = 1;

/// `body` as a cursor: its JSON, base64url without padding.
fn seal<T: Serialize>(body: &T) -> String {
    let json = serde_json::to_vec(body).expect("a cursor body always serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// The body `cursor` seals, or `None` for anything that is not base64url JSON
/// of a `T` -- each caller refuses that as malformed, in its own words.
fn open<T: DeserializeOwned>(cursor: &str) -> Option<T> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    serde_json::from_slice(&bytes).ok()
}
