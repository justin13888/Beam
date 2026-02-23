use chrono::{DateTime, Utc};
use std::path::PathBuf;
use uuid::Uuid;

/// A media library containing movies and/or TV shows
#[derive(Debug, Clone)]
pub struct Library {
    pub id: Uuid,
    pub name: String,
    pub root_path: PathBuf,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_scan_started_at: Option<DateTime<Utc>>,
    pub last_scan_finished_at: Option<DateTime<Utc>>,
    pub last_scan_file_count: Option<i32>,
}

/// Parameters for creating a new library
#[derive(Debug, Clone)]
pub struct CreateLibrary {
    pub name: String,
    pub root_path: PathBuf,
    pub description: Option<String>,
}

impl From<beam_entity::library::Model> for Library {
    fn from(model: beam_entity::library::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            root_path: PathBuf::from(model.root_path),
            description: model.description,
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
            last_scan_started_at: model.last_scan_started_at.map(|d| d.with_timezone(&Utc)),
            last_scan_finished_at: model.last_scan_finished_at.map(|d| d.with_timezone(&Utc)),
            last_scan_file_count: model.last_scan_file_count,
        }
    }
}
