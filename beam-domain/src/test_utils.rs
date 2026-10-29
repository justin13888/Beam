//! Test data builders for the domain types tests otherwise spell out field by
//! field (NFR-207): a plausible value of each, for a test to adjust with
//! struct-update syntax.
//!
//! One module rather than a `test_utils` beside each type, because
//! [`crate::models`] glob re-exports every model module: a second
//! `models::*::test_utils` would make the name ambiguous there.

use std::path::PathBuf;

use chrono::DateTime;
use uuid::Uuid;

use crate::models::enrichment::{
    EnrichmentState, EnrichmentStatus, EnrichmentTargetId, FieldLocks,
};
use crate::models::file::{CreateMediaFile, FileStatus, MediaFile};
use crate::models::library::{CreateLibrary, Library};

/// A one-gigabyte, unclassified Matroska file at `path` in `library_id`,
/// never probed: no duration, identity or tags, and `Unknown`, as the `files`
/// CHECK requires of a row with no content.
#[must_use]
pub fn create_media_file(library_id: Uuid, path: impl Into<PathBuf>) -> CreateMediaFile {
    CreateMediaFile {
        library_id,
        path: path.into(),
        hash: 0x5eed,
        size_bytes: 1_000_000_000,
        mtime: None,
        identity: None,
        mime_type: Some("video/x-matroska".to_string()),
        duration: None,
        container_format: Some("matroska".to_string()),
        content: None,
        status: FileStatus::Unknown,
        classifier_version: 0,
        container_tags: None,
    }
}

/// The row [`create_media_file`] stores, under a fresh id, scanned and
/// updated at the Unix epoch, and present.
#[must_use]
pub fn media_file(library_id: Uuid, path: impl Into<PathBuf>) -> MediaFile {
    let CreateMediaFile {
        library_id,
        path,
        hash,
        size_bytes,
        mtime,
        identity,
        mime_type,
        duration,
        container_format,
        content,
        status,
        classifier_version,
        container_tags,
    } = create_media_file(library_id, path);
    MediaFile {
        id: Uuid::new_v4(),
        library_id,
        path,
        hash,
        size_bytes,
        mtime,
        identity,
        mime_type,
        duration,
        container_format,
        content,
        status,
        scanned_at: DateTime::UNIX_EPOCH,
        updated_at: DateTime::UNIX_EPOCH,
        missing_since: None,
        classifier_version,
        container_tags,
    }
}

/// A library called `name`, rooted at `/media/{name}`, with no description.
#[must_use]
pub fn create_library(name: &str) -> CreateLibrary {
    CreateLibrary {
        name: name.to_string(),
        root_path: PathBuf::from(format!("/media/{name}")),
        description: None,
    }
}

/// The library [`create_library`] makes, under a fresh id, created at the
/// Unix epoch and never scanned.
#[must_use]
pub fn library(name: &str) -> Library {
    let CreateLibrary {
        name,
        root_path,
        description,
    } = create_library(name);
    Library {
        id: Uuid::new_v4(),
        name,
        root_path,
        description,
        created_at: DateTime::UNIX_EPOCH,
        updated_at: DateTime::UNIX_EPOCH,
        last_scan_started_at: None,
        last_scan_finished_at: None,
        last_scan_file_count: None,
    }
}

/// A freshly queued enrichment row for `target`: `Pending`, never attempted,
/// unmatched, nothing locked, last changed at the Unix epoch.
#[must_use]
pub fn enrichment_state(target: EnrichmentTargetId) -> EnrichmentState {
    EnrichmentState {
        id: Uuid::new_v4(),
        target,
        status: EnrichmentStatus::Pending,
        attempts: 0,
        next_attempt_at: None,
        enriched_at: None,
        match_confidence: None,
        matched_ref: None,
        force_refresh: false,
        last_error: None,
        locked_fields: FieldLocks::none(),
        updated_at: DateTime::UNIX_EPOCH,
    }
}
