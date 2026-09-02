use std::path::{Path, PathBuf};
use std::sync::Arc;

use sea_orm::DbErr;
use thiserror::Error;
use tracing::{error, warn};
use uuid::Uuid;

use crate::models::{Library, LibraryFile};
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use beam_domain::models::Library as DomainLibrary;
use beam_index::services::index::{IndexError, IndexService};

pub trait PathValidator: Send + Sync + std::fmt::Debug {
    /// Validates that `requested` is within `root`, returning the canonical absolute path.
    /// Returns `LibraryError::PathNotFound` if the path does not exist.
    /// Returns `LibraryError::PathOutsideRoot` if the path escapes root.
    fn validate_library_path(&self, requested: &Path, root: &Path)
    -> Result<PathBuf, LibraryError>;
}

#[derive(Debug)]
pub struct OsPathValidator;

impl PathValidator for OsPathValidator {
    fn validate_library_path(
        &self,
        requested: &Path,
        root: &Path,
    ) -> Result<PathBuf, LibraryError> {
        // The messages below reach an administrator's browser as the `detail`
        // of a 400, so none of them names a filesystem path (NFR-108). The
        // paths go to the log, which is where the operator who can act on them
        // is looking.
        let canonical_root = root.canonicalize().map_err(|e| {
            error!(root = %root.display(), error = %e, "failed to canonicalize video_dir");
            LibraryError::PathNotFound("Video directory is not accessible".to_string())
        })?;

        let target_path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            root.join(requested)
        };

        let canonical_target = target_path.canonicalize().map_err(|e| {
            warn!(requested = %target_path.display(), error = %e, "library path does not resolve");
            LibraryError::PathNotFound("Library path does not exist".to_string())
        })?;

        if !canonical_target.starts_with(&canonical_root) {
            warn!(
                requested = %canonical_target.display(),
                root = %canonical_root.display(),
                "library path resolves outside the video directory"
            );
            return Err(LibraryError::PathOutsideRoot(
                "Library path must be within the configured video directory".to_string(),
            ));
        }

        Ok(canonical_target)
    }
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    #[derive(Debug, Clone)]
    pub enum InMemoryPathValidatorResult {
        Success(PathBuf),
        PathNotFound(String),
        PathOutsideRoot(String),
    }

    #[derive(Debug)]
    pub struct InMemoryPathValidator {
        result: InMemoryPathValidatorResult,
    }

    impl InMemoryPathValidator {
        pub fn success(path: PathBuf) -> Self {
            Self {
                result: InMemoryPathValidatorResult::Success(path),
            }
        }

        pub fn path_not_found(msg: impl Into<String>) -> Self {
            Self {
                result: InMemoryPathValidatorResult::PathNotFound(msg.into()),
            }
        }

        pub fn path_outside_root(msg: impl Into<String>) -> Self {
            Self {
                result: InMemoryPathValidatorResult::PathOutsideRoot(msg.into()),
            }
        }
    }

    impl PathValidator for InMemoryPathValidator {
        fn validate_library_path(
            &self,
            _requested: &Path,
            _root: &Path,
        ) -> Result<PathBuf, LibraryError> {
            match &self.result {
                InMemoryPathValidatorResult::Success(path) => Ok(path.clone()),
                InMemoryPathValidatorResult::PathNotFound(msg) => {
                    Err(LibraryError::PathNotFound(msg.clone()))
                }
                InMemoryPathValidatorResult::PathOutsideRoot(msg) => {
                    Err(LibraryError::PathOutsideRoot(msg.clone()))
                }
            }
        }
    }
}

// Re-exported at the module root so the doubles keep the paths they had before
// they moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::{InMemoryPathValidator, InMemoryPathValidatorResult};

#[async_trait::async_trait]
pub trait LibraryService: Send + Sync + std::fmt::Debug {
    /// Get all libraries by user ID
    /// Returns None if user is not found
    async fn get_libraries(&self, user_id: String) -> Result<Vec<Library>, LibraryError>;

    /// Get a single library by ID
    async fn get_library_by_id(&self, library_id: String) -> Result<Option<Library>, LibraryError>;

    /// Get all files within a library
    async fn get_library_files(&self, library_id: String)
    -> Result<Vec<LibraryFile>, LibraryError>;

    /// Get a single file by its ID.
    ///
    /// A `file_id` that is not a UUID is [`LibraryError::InvalidId`], not
    /// `Ok(None)`: the caller sent something malformed rather than named a
    /// file that does not exist, and the delivery routes answer the two
    /// differently (400 against 404).
    async fn get_file_by_id(&self, file_id: String) -> Result<Option<LibraryFile>, LibraryError>;

    /// Create a new library
    async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<Library, LibraryError>;

    /// Scan a library for new content
    async fn scan_library(&self, library_id: String) -> Result<u32, LibraryError>;

    /// Delete a library by ID
    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError>;
}

#[derive(Debug)]
pub struct LocalLibraryService {
    library_repo: Arc<dyn beam_domain::repositories::LibraryRepository>,
    file_repo: Arc<dyn beam_domain::repositories::FileRepository>,
    video_dir: PathBuf,
    notification_service: Arc<dyn NotificationService>,
    index_service: Arc<dyn IndexService>,
    path_validator: Arc<dyn PathValidator>,
}

impl LocalLibraryService {
    pub fn new(
        library_repo: Arc<dyn beam_domain::repositories::LibraryRepository>,
        file_repo: Arc<dyn beam_domain::repositories::FileRepository>,
        video_dir: PathBuf,
        notification_service: Arc<dyn NotificationService>,
        index_service: Arc<dyn IndexService>,
        path_validator: Arc<dyn PathValidator>,
    ) -> Self {
        LocalLibraryService {
            library_repo,
            file_repo,
            video_dir,
            notification_service,
            index_service,
            path_validator,
        }
    }
}

#[async_trait::async_trait]
impl LibraryService for LocalLibraryService {
    async fn get_libraries(&self, _user_id: String) -> Result<Vec<Library>, LibraryError> {
        let domain_libraries = self.library_repo.find_all().await?;

        let mut result = Vec::new();
        for lib in domain_libraries {
            let DomainLibrary {
                id,
                name,
                root_path: _,
                description,
                created_at: _,
                updated_at: _,
                last_scan_started_at,
                last_scan_finished_at,
                last_scan_file_count,
            } = lib;
            let size = self.library_repo.count_files(lib.id).await?;

            result.push(Library {
                id: id.to_string(),
                name,
                description,
                size: size as u32,
                last_scan_started_at: last_scan_started_at.map(|d| d.with_timezone(&chrono::Utc)),
                last_scan_finished_at: last_scan_finished_at.map(|d| d.with_timezone(&chrono::Utc)),
                last_scan_file_count,
            });
        }

        Ok(result)
    }

    async fn get_library_by_id(&self, library_id: String) -> Result<Option<Library>, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;
        let library = self.library_repo.find_by_id(lib_uuid).await?;

        match library {
            Some(lib) => {
                let size = self.library_repo.count_files(lib.id).await?;
                Ok(Some(Library {
                    id: lib.id.to_string(),
                    name: lib.name,
                    description: lib.description,
                    size: size as u32,
                    last_scan_started_at: lib.last_scan_started_at,
                    last_scan_finished_at: lib.last_scan_finished_at,
                    last_scan_file_count: lib.last_scan_file_count,
                }))
            }
            None => Ok(None),
        }
    }

    async fn get_library_files(
        &self,
        library_id: String,
    ) -> Result<Vec<LibraryFile>, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;

        self.library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        let files = self.file_repo.find_all_by_library(lib_uuid).await?;
        Ok(files.into_iter().map(LibraryFile::from).collect())
    }

    async fn get_file_by_id(&self, file_id: String) -> Result<Option<LibraryFile>, LibraryError> {
        let file_uuid = Uuid::parse_str(&file_id).map_err(|_| LibraryError::InvalidId)?;
        let file = self.file_repo.find_by_id(file_uuid).await?;
        Ok(file.map(LibraryFile::from))
    }

    async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<Library, LibraryError> {
        use beam_domain::models::CreateLibrary;

        let requested_path = PathBuf::from(&root_path);

        let canonical_target = self
            .path_validator
            .validate_library_path(&requested_path, &self.video_dir)?;

        let create = CreateLibrary {
            name: name.clone(),
            root_path: canonical_target,
            description: None,
        };

        let DomainLibrary {
            id,
            name,
            root_path: _,
            description,
            created_at: _,
            updated_at: _,
            last_scan_started_at,
            last_scan_finished_at,
            last_scan_file_count,
        } = self.library_repo.create(create).await?;

        self.notification_service.publish(AdminEvent::info(
            EventCategory::System,
            format!("Library '{}' created", name),
            Some(id.to_string()),
            Some(name.clone()),
        ));

        Ok(Library {
            id: id.to_string(),
            name,
            description,
            size: 0,
            last_scan_started_at,
            last_scan_finished_at,
            last_scan_file_count,
        })
    }

    async fn scan_library(&self, library_id: String) -> Result<u32, LibraryError> {
        self.index_service
            .scan_library(library_id)
            .await
            .map_err(LibraryError::from)
    }

    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;

        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        self.library_repo.delete(lib_uuid).await?;

        self.notification_service.publish(AdminEvent::info(
            EventCategory::System,
            format!("Library '{}' deleted", library.name),
            Some(lib_uuid.to_string()),
            Some(library.name),
        ));

        Ok(true)
    }
}

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("Database error: {0}")]
    Db(#[from] DbErr),
    #[error("Library not found")]
    LibraryNotFound,
    #[error("Invalid Library ID")]
    InvalidId,
    #[error("Path not found: {0}")]
    PathNotFound(String),
    #[error("Library path is outside the permitted root: {0}")]
    PathOutsideRoot(String),
}

impl From<IndexError> for LibraryError {
    fn from(e: IndexError) -> Self {
        match e {
            IndexError::Db(db_err) => LibraryError::Db(db_err),
            IndexError::LibraryNotFound => LibraryError::LibraryNotFound,
            IndexError::InvalidId => LibraryError::InvalidId,
            IndexError::PathNotFound(s) => LibraryError::PathNotFound(s),
        }
    }
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod library_tests;
