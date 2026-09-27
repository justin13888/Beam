use std::path::{Path, PathBuf};
use std::sync::Arc;

use sea_orm::DbErr;
use thiserror::Error;
use tracing::{error, warn};
use uuid::Uuid;

use crate::models::{Library, LibraryFile, ScanJob};
use crate::services::notification::{AdminEvent, EventCategory, NotificationService};
use beam_domain::models::Library as DomainLibrary;
use beam_index::runtime::LibraryWatchHook;
use beam_index::services::index::{IndexError, IndexService};
use beam_index::services::scan::ScanTrigger;

pub trait PathValidator: Send + Sync + std::fmt::Debug {
    /// Validates a *library root* at registration, returning the canonical
    /// absolute path.
    ///
    /// Named for the root deliberately: the rules below are the ones a root has
    /// to satisfy, not the ones an arbitrary path inside a library does. Beam
    /// never validates the latter -- file references are resolved server-side
    /// from the catalog (NFR-601) -- so nothing here has to hold for a media
    /// file.
    ///
    /// Returns `LibraryError::PathNotFound` if the root does not resolve, or
    /// resolves to something that is not a directory.
    /// Returns `LibraryError::PathOutsideRoot` if the root escapes `root`.
    fn validate_library_root(&self, requested: &Path, root: &Path)
    -> Result<PathBuf, LibraryError>;
}

#[derive(Debug)]
pub struct OsPathValidator;

impl PathValidator for OsPathValidator {
    fn validate_library_root(
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

        // A scan refuses a root that is not a directory, so accepting one here
        // would register a library that returns 201 and then fails every scan
        // for the rest of its life. Refuse it at the point the operator can
        // still correct the path.
        if !canonical_target.is_dir() {
            warn!(
                requested = %canonical_target.display(),
                "library root is not a directory"
            );
            return Err(LibraryError::PathNotFound(
                "Library root path is not a directory".to_string(),
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
        fn validate_library_root(
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

/// What a candidate library root collides with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootConflict {
    /// The candidate is, contains, or lies inside this existing library root.
    Library(PathBuf),
    /// The candidate is, contains, or lies inside the server's data directory.
    DataDir,
}

/// Whether a candidate library root overlaps an existing library root or the
/// data directory.
///
/// Overlap in either direction is a conflict. A root inside another library
/// indexes the same files twice, as two libraries with two sets of rows and
/// two watches; a root containing another does the same from the other side.
/// A root holding the data directory would index Beam's own artwork cache and
/// have Beam writing under a library root, which FR-202 forbids; a root inside
/// the data directory is Beam's state, not media.
///
/// Every path must already be canonical: `Path::starts_with` compares whole
/// components, so `/m/movies` does not contain `/m/movies2`, but it cannot see
/// through a `..` or a symlink.
pub fn find_root_conflict(
    candidate: &Path,
    existing: &[PathBuf],
    data_dir: &Path,
) -> Option<RootConflict> {
    if roots_overlap(candidate, data_dir) {
        return Some(RootConflict::DataDir);
    }
    existing
        .iter()
        .find(|root| roots_overlap(candidate, root))
        .map(|root| RootConflict::Library(root.clone()))
}

/// Whether one canonical path is, contains, or lies inside the other.
fn roots_overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Two registered libraries whose roots overlap. Registration refuses this
/// now, but a library registered before it did may still be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingLibraryOverlap {
    pub first: Uuid,
    pub second: Uuid,
}

/// Why the stored libraries stop the server from starting.
#[derive(Debug, Error)]
pub enum StartupRootError {
    #[error("failed to list libraries to check BEAM_DATA_DIR against them: {0}")]
    Db(#[from] DbErr),
    /// The data directory is, contains, or lies inside a library root. Beam
    /// writes its artwork cache there, and it must never write under a
    /// library (FR-202), so this is fatal rather than a warning.
    #[error(
        "BEAM_DATA_DIR ({data_dir}) overlaps the root of library '{library}' ({root}); \
         Beam never writes inside a library, so move BEAM_DATA_DIR outside every library root \
         and restart"
    )]
    DataDirOverlapsLibrary {
        data_dir: PathBuf,
        library: String,
        root: PathBuf,
    },
}

/// Check the stored library roots against the data directory and each other,
/// once at startup.
///
/// Registration refuses both overlaps, but it cannot see a data directory
/// that is moved *into* an existing library afterwards, nor libraries stored
/// before it refused them. The first is fatal: starting would write the
/// artwork cache under a library root. The second only warns: those
/// installations work today, and refusing to start would take them down over
/// a condition that costs duplicated rows, not correctness. The overlaps are
/// returned so the caller (and a test) can see what was reported.
///
/// `data_dir` must be canonical; stored roots already are.
pub async fn audit_existing_roots(
    library_repo: &dyn beam_domain::repositories::LibraryRepository,
    data_dir: &Path,
) -> Result<Vec<ExistingLibraryOverlap>, StartupRootError> {
    let libraries = library_repo.find_all().await?;
    if let Some(library) = libraries
        .iter()
        .find(|library| roots_overlap(&library.root_path, data_dir))
    {
        return Err(StartupRootError::DataDirOverlapsLibrary {
            data_dir: data_dir.to_path_buf(),
            library: library.name.clone(),
            root: library.root_path.clone(),
        });
    }

    let mut overlaps = Vec::new();
    for (index, first) in libraries.iter().enumerate() {
        for second in &libraries[index + 1..] {
            if roots_overlap(&first.root_path, &second.root_path) {
                warn!(
                    first = %first.id,
                    first_root = %first.root_path.display(),
                    second = %second.id,
                    second_root = %second.root_path.display(),
                    "two libraries overlap; their shared files are indexed twice. \
                     Delete one and register a disjoint root"
                );
                overlaps.push(ExistingLibraryOverlap {
                    first: first.id,
                    second: second.id,
                });
            }
        }
    }
    Ok(overlaps)
}

/// A catalogued file resolved to where it lives on disk.
///
/// Server-internal on purpose: it carries the absolute path the delivery
/// routes open, which NFR-108 keeps out of every client-facing response. It is
/// deliberately neither `Serialize` nor a `Schema`, so it cannot be returned
/// from a handler by accident; the client-facing view of a file is
/// [`LibraryFile`], whose path is relative to the library root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedFile {
    pub id: Uuid,
    /// Absolute path of the file on the server's filesystem.
    pub path: PathBuf,
    /// Detected MIME type (e.g. "video/mp4"), if known.
    pub mime_type: Option<String>,
}

impl From<beam_domain::models::MediaFile> for LocatedFile {
    fn from(file: beam_domain::models::MediaFile) -> Self {
        let beam_domain::models::MediaFile {
            id,
            library_id: _,
            path,
            hash: _,
            size_bytes: _,
            mtime: _,
            mime_type,
            duration: _,
            container_format: _,
            status: _,
            content: _,
            scanned_at: _,
            updated_at: _,
            missing_since: _,
            classifier_version: _,
        } = file;
        LocatedFile {
            id,
            path,
            mime_type,
        }
    }
}

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
    async fn get_file_by_id(&self, file_id: String) -> Result<Option<LocatedFile>, LibraryError>;

    /// Create a new library, watched by the filesystem watcher from the
    /// moment it exists.
    async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<Library, LibraryError>;

    /// Start a scan of a library, returning its job as registered: queued.
    /// The scan runs on a task of its own; follow it with
    /// [`Self::get_scan`]. Fails with [`LibraryError::ScanInProgress`] while
    /// a scan of the library is queued or running, and with
    /// [`LibraryError::PathNotFound`] when its root is not a directory.
    async fn start_scan(&self, library_id: Uuid) -> Result<ScanJob, LibraryError>;

    /// The latest scan of a library in this process: `None` when there has
    /// been none since the server started.
    async fn get_scan(&self, library_id: Uuid) -> Result<Option<ScanJob>, LibraryError>;

    /// Delete a library by ID. A scan of it that is queued or running is
    /// cancelled and waited for first, and no new one starts (see
    /// [`IndexService::stop_scan`]); the indexer forgets the library's latest
    /// job after, and the watcher stops watching it.
    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError>;
}

#[derive(Debug)]
pub struct LocalLibraryService {
    library_repo: Arc<dyn beam_domain::repositories::LibraryRepository>,
    file_repo: Arc<dyn beam_domain::repositories::FileRepository>,
    video_dir: PathBuf,
    /// The server's data directory, canonical. No library root may overlap it.
    data_dir: PathBuf,
    notification_service: Arc<dyn NotificationService>,
    index_service: Arc<dyn IndexService>,
    path_validator: Arc<dyn PathValidator>,
    /// Told when a library is created or deleted, so the filesystem watcher
    /// follows at once (issue #180).
    watch_hook: Arc<dyn LibraryWatchHook>,
}

impl LocalLibraryService {
    /// `data_dir` must be canonical: it is compared component-wise against
    /// canonical library roots.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        library_repo: Arc<dyn beam_domain::repositories::LibraryRepository>,
        file_repo: Arc<dyn beam_domain::repositories::FileRepository>,
        video_dir: PathBuf,
        data_dir: PathBuf,
        notification_service: Arc<dyn NotificationService>,
        index_service: Arc<dyn IndexService>,
        path_validator: Arc<dyn PathValidator>,
        watch_hook: Arc<dyn LibraryWatchHook>,
    ) -> Self {
        LocalLibraryService {
            library_repo,
            file_repo,
            video_dir,
            data_dir,
            notification_service,
            index_service,
            path_validator,
            watch_hook,
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

        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        // Reported relative to the library's root, never as the absolute path
        // the file is stored under (NFR-108).
        let files = self.file_repo.find_all_by_library(lib_uuid).await?;
        Ok(files
            .into_iter()
            .map(|file| LibraryFile::from_domain(file, &library.root_path))
            .collect())
    }

    async fn get_file_by_id(&self, file_id: String) -> Result<Option<LocatedFile>, LibraryError> {
        let file_uuid = Uuid::parse_str(&file_id).map_err(|_| LibraryError::InvalidId)?;
        let file = self.file_repo.find_by_id(file_uuid).await?;
        Ok(file.map(LocatedFile::from))
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
            .validate_library_root(&requested_path, &self.video_dir)?;

        // As with the validator, the rejections name no path (NFR-108); the
        // log does.
        let existing_roots: Vec<PathBuf> = self
            .library_repo
            .find_all()
            .await?
            .into_iter()
            .map(|library| library.root_path)
            .collect();
        match find_root_conflict(&canonical_target, &existing_roots, &self.data_dir) {
            None => {}
            Some(RootConflict::DataDir) => {
                warn!(
                    requested = %canonical_target.display(),
                    data_dir = %self.data_dir.display(),
                    "library path overlaps the data directory"
                );
                return Err(LibraryError::PathOverlapsDataDir);
            }
            Some(RootConflict::Library(existing)) => {
                warn!(
                    requested = %canonical_target.display(),
                    existing = %existing.display(),
                    "library path overlaps an existing library"
                );
                return Err(LibraryError::PathOverlapsLibrary);
            }
        }

        let create = CreateLibrary {
            name: name.clone(),
            root_path: canonical_target,
            description: None,
        };

        let created = self.library_repo.create(create).await?;
        // Watched from now, not from the next maintenance cycle. A failure
        // is logged by the hook, and the cycle registers it instead.
        self.watch_hook.library_created(&created).await;
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
        } = created;

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

    async fn start_scan(&self, library_id: Uuid) -> Result<ScanJob, LibraryError> {
        let ticket = self
            .index_service
            .begin_scan(library_id, ScanTrigger::Manual)
            .await?;
        let job = ticket.job();
        // The request answers now; the scan runs as long as it runs. A task
        // that dies unfinished fails the job as interrupted.
        let index_service = self.index_service.clone();
        tokio::spawn(async move {
            if let Err(e) = index_service.run_scan(ticket).await {
                warn!(library_id = %library_id, error = %e, "library scan failed");
            }
        });
        Ok(ScanJob::from(job))
    }

    async fn get_scan(&self, library_id: Uuid) -> Result<Option<ScanJob>, LibraryError> {
        self.library_repo
            .find_by_id(library_id)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;
        Ok(self.index_service.scan_job(library_id).map(ScanJob::from))
    }

    async fn delete_library(&self, library_id: String) -> Result<bool, LibraryError> {
        let lib_uuid = Uuid::parse_str(&library_id).map_err(|_| LibraryError::InvalidId)?;

        let library = self
            .library_repo
            .find_by_id(lib_uuid)
            .await?
            .ok_or(LibraryError::LibraryNotFound)?;

        // A scan stops after the file it is on and fails as cancelled. It is
        // waited for, so it never writes files or titles for a library whose
        // rows are going -- which would leave orphaned titles and fail the
        // job as an internal error. A scan held on one file past the timeout
        // does not hold the delete: whatever it then writes is refused with
        // the library, or left for the orphan sweep.
        if !self.index_service.stop_scan(lib_uuid).await {
            warn!(
                library_id = %lib_uuid,
                "a scan of the library did not stop in time; deleting the library anyway"
            );
        }
        self.library_repo.delete(lib_uuid).await?;
        self.index_service.forget_library(lib_uuid);
        self.watch_hook.library_deleted(lib_uuid).await;

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
    // Unit variants: the message is the whole story, and names no path
    // (NFR-108).
    #[error("Library path is, contains, or lies inside an existing library")]
    PathOverlapsLibrary,
    #[error("Library path overlaps the server's data directory")]
    PathOverlapsDataDir,
    #[error("A scan of this library is already queued or running")]
    ScanInProgress,
}

impl From<IndexError> for LibraryError {
    fn from(e: IndexError) -> Self {
        match e {
            IndexError::Db(db_err) => LibraryError::Db(db_err),
            IndexError::LibraryNotFound => LibraryError::LibraryNotFound,
            IndexError::InvalidId => LibraryError::InvalidId,
            IndexError::PathNotFound(s) => LibraryError::PathNotFound(s),
            IndexError::ScanInProgress => LibraryError::ScanInProgress,
            // Unreachable: a cancelled scan is reported through its job,
            // never by the call that starts it. An internal error rather
            // than a plausible 4xx if that ever changes.
            IndexError::Cancelled => LibraryError::Db(DbErr::Custom(e.to_string())),
        }
    }
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod library_tests;
