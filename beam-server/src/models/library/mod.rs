mod file;

use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::Serialize;
use uuid::Uuid;

pub use file::*;

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct Library {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// How many indexed files the library holds now.
    pub file_count: u64,
    /// When the last scan started
    pub last_scan_started_at: Option<DateTime<Utc>>,
    /// When the last scan finished
    pub last_scan_finished_at: Option<DateTime<Utc>>,
    /// Number of files found in the last scan
    pub last_scan_file_count: Option<u64>,
}

impl Library {
    /// The client-facing view of `library`, which holds `file_count` files.
    pub fn from_domain(library: beam_domain::models::Library, file_count: u64) -> Self {
        let beam_domain::models::Library {
            id,
            name,
            // Never exposed (NFR-108).
            root_path: _,
            description,
            created_at: _,
            updated_at: _,
            last_scan_started_at,
            last_scan_finished_at,
            last_scan_file_count,
        } = library;
        Self {
            id,
            name,
            description,
            file_count,
            last_scan_started_at,
            last_scan_finished_at,
            // The column is a signed `integer` because Postgres has no
            // unsigned one; a count is never negative, and one that somehow
            // were is reported as unknown rather than wrapped.
            last_scan_file_count: last_scan_file_count.and_then(|count| u64::try_from(count).ok()),
        }
    }
}
