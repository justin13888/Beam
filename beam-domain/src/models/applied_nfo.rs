//! What an NFO held when Beam last applied it (issue #184, FR-219).
//!
//! An NFO is (re)applied exactly when what it holds differs from what Beam
//! recorded the last time it applied it -- never by comparing its
//! modification time against a scan's, which a `cp -p`, a skewed NAS clock or
//! a scan killed half-way all defeat. What it holds is its size and a hash of
//! its content (an NFO is at most [`crate::utils::nfo::MAX_NFO_BYTES`], so
//! hashing one is cheap). The stat stamp beside them is only a shortcut: an
//! NFO whose stamp is unchanged is not read again.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The record of the NFO at `path`, as Beam last applied it.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedNfo {
    pub id: Uuid,
    pub library_id: Uuid,
    pub path: PathBuf,
    /// The NFO's size in bytes when it was applied.
    pub size_bytes: u64,
    /// A hash of the NFO's content when it was applied.
    pub content_hash: String,
    /// What a stat said about the NFO when it was read (its size, and its
    /// modification and change times), or `None` where the platform has no
    /// change time. The same stamp means the file was not written since, so
    /// it need not be read to know its content is unchanged.
    pub change_stamp: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An NFO as Beam just read and applied it, keyed by its path.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordAppliedNfo {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub content_hash: String,
    pub change_stamp: Option<String>,
}

impl RecordAppliedNfo {
    /// Whether `stored` recorded the same content as this: the NFO has not
    /// changed since it was last applied.
    pub fn same_content(&self, stored: &AppliedNfo) -> bool {
        stored.size_bytes == self.size_bytes && stored.content_hash == self.content_hash
    }

    /// Whether `stored` already records exactly this -- so nothing need be
    /// written.
    pub fn matches(&self, stored: &AppliedNfo) -> bool {
        let RecordAppliedNfo {
            library_id,
            path,
            size_bytes: _,
            content_hash: _,
            change_stamp,
        } = self;
        self.same_content(stored)
            && stored.library_id == *library_id
            && stored.path == *path
            && stored.change_stamp == *change_stamp
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::applied_nfo::Model> for AppliedNfo {
    fn from(model: beam_entity::applied_nfo::Model) -> Self {
        let beam_entity::applied_nfo::Model {
            id,
            library_id,
            path,
            size_bytes,
            content_hash,
            change_stamp,
            created_at,
            updated_at,
        } = model;
        Self {
            id,
            library_id,
            path: PathBuf::from(path),
            size_bytes: size_bytes.max(0) as u64,
            content_hash,
            change_stamp,
            created_at: created_at.with_timezone(&Utc),
            updated_at: updated_at.with_timezone(&Utc),
        }
    }
}
