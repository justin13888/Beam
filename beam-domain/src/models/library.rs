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

/// How many live titles one library holds, by kind (issue #494): what
/// [`crate::repositories::LibraryRepository::count_titles`] answers.
///
/// A title counts as [`crate::models::LibraryShape`] counts it, scoped to the
/// library's own files: while at least one of them is present -- a missing
/// file (issue #179) keeps nothing live. A show spanning two libraries counts
/// in each library that holds a present file of one of its episodes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryTitleCounts {
    /// Movies with a present file in the library.
    pub movies: u64,
    /// Shows with an episode that has a present file in the library.
    pub shows: u64,
    /// Seasons with an episode that has a present file in the library.
    pub seasons: u64,
    /// Episodes with a present file in the library.
    pub episodes: u64,
}

/// Parameters for creating a new library
#[derive(Debug, Clone)]
pub struct CreateLibrary {
    pub name: String,
    pub root_path: PathBuf,
    pub description: Option<String>,
}

#[cfg(feature = "entity")]
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
