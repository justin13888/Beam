mod file;

use async_graphql::SimpleObject;
use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::Serialize;

pub use file::*;

#[derive(Clone, Debug, Serialize, ToSchema, SimpleObject)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Number of media items in the library
    pub size: u32,
    /// When the last scan started
    pub last_scan_started_at: Option<DateTime<Utc>>,
    /// When the last scan finished
    pub last_scan_finished_at: Option<DateTime<Utc>>,
    /// Number of files found in the last scan
    pub last_scan_file_count: Option<i32>,
}
