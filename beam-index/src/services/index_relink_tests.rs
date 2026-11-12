//! Moved and renamed files keep their rows (issue #180).
//!
//! A file that vanished from one path and a new file with the same content at
//! another, in the same library, are one file: its row is pointed at the new
//! path, keeping its id -- and so its playback progress, its movie or
//! episode, and its streams -- and the file is neither probed nor classified
//! again. Directory events reconcile the directory's subtree.
//!
//! The filesystem is a real `TempDir` and the hasher the real one, so a
//! "same file" is a file with the same bytes. The prober is a counting
//! double: no relink may probe. This file holds the harness; the planner
//! tables and the scan and watcher tests are its `planner` and
//! `integration` modules.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::library_file::test_support::snapshot;
use crate::probe::metadata::MetadataError;
use crate::services::filesystem_probe::{FilesystemKind, FixedFilesystemProbe};
use crate::services::hash::{HashConfig, LocalHashService};
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::CreateLibrary;
use beam_domain::models::admin_log::AdminLog;
use beam_domain::models::watch_state::{RecordProgress, WatchTarget};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::WatchStateRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

fn instant(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid instant")
}

/// A prober that reports an hour-long matroska file, or fails, counting its
/// calls: a relinked file is never probed.
#[derive(Debug, Default)]
struct CountingProber {
    calls: AtomicUsize,
    fails: AtomicBool,
}

#[async_trait::async_trait]
impl MediaInfoService for CountingProber {
    async fn get_video_metadata(
        &self,
        file: LibraryFile,
    ) -> Result<VideoFileMetadata, MetadataError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fails.load(Ordering::SeqCst) {
            return Err(MetadataError::UnknownError(
                "moov atom not found".to_string(),
            ));
        }
        Ok(VideoFileMetadata {
            file_path: file.path().to_path_buf(),
            metadata: HashMap::default(),
            best_video_stream: None,
            best_audio_stream: None,
            best_subtitle_stream: None,
            duration: Duration::from_secs(60 * 60).as_micros() as i64,
            streams: vec![],
            format_name: "matroska".to_string(),
            format_long_name: "Matroska".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
            chapters: Vec::new(),
        })
    }
}

/// The real hasher, counting its calls.
#[derive(Debug)]
struct CountingHasher {
    inner: LocalHashService,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl HashService for CountingHasher {
    async fn hash_async(&self, file: std::fs::File) -> std::io::Result<u64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.hash_async(file).await
    }
}

struct Harness {
    dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    notifications: Arc<InMemoryNotificationService>,
    prober: Arc<CountingProber>,
    hasher: Arc<CountingHasher>,
    progress: Arc<InMemoryWatchStateRepository>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        Self::with_service(|service| service).await
    }

    /// A harness whose service `configure` finishes building.
    async fn with_service(configure: impl FnOnce(LocalIndexService) -> LocalIndexService) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let notifications = Arc::new(InMemoryNotificationService::new());
        let prober = Arc::new(CountingProber::default());
        let hasher = Arc::new(CountingHasher {
            inner: LocalHashService::new(HashConfig { num_threads: 1 }),
            calls: AtomicUsize::new(0),
        });
        let clock = Arc::new(TestClock::starting_at(instant(0)));
        let progress = Arc::new(InMemoryWatchStateRepository::new(clock.clone()));
        let library = library_repo
            .create(CreateLibrary {
                name: "Moves".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let service = LocalIndexService::new(
            library_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryMediaStreamRepository::default()),
            hasher.clone(),
            prober.clone(),
            notifications.clone(),
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>,
            progress.clone(),
        )
        .with_clock(clock)
        // A local filesystem, whatever the temporary directory is on: its
        // inode numbers are compared unless `configure` says otherwise.
        .with_filesystem_probe(Arc::new(FixedFilesystemProbe(FilesystemKind::Local)));
        let service = configure(service);
        Self {
            dir,
            root,
            library,
            library_repo,
            file_repo,
            admin_log_repo,
            notifications,
            prober,
            hasher,
            progress,
            service,
        }
    }

    /// Record that someone watched `path`'s file.
    async fn watch(&self, path: &Path) {
        let file = self.present(path).await;
        self.progress
            .record_progress(RecordProgress {
                user_id: Uuid::new_v4(),
                // The title does not matter to the indexer: only the file
                // the report named does.
                target: WatchTarget::Movie {
                    movie_id: Uuid::new_v4(),
                },
                file_id: file.id,
                position_secs: 600.0,
                duration_secs: Some(3600.0),
                finishes_title: true,
            })
            .await
            .unwrap();
    }

    /// Write `rel` under the root with `content` as its bytes: two files with
    /// the same content are the same file to the indexer.
    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Move `from` to `rel` under the root.
    fn mv(&self, from: &Path, rel: &str) -> PathBuf {
        let to = self.root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(from, &to).unwrap();
        to
    }

    async fn scan(&self) -> ScanProgress {
        self.scan_library(self.library.id).await
    }

    async fn scan_library(&self, library_id: Uuid) -> ScanProgress {
        self.service
            .scan_now(library_id, ScanTrigger::Manual)
            .await
            .expect("the scan runs")
    }

    async fn reconcile(&self, path: &Path, kind: FsEventKind) -> ReconcileOutcome {
        self.service
            .reconcile_path(self.library.id, path.to_path_buf(), kind)
            .await
            .expect("the event reconciles")
    }

    async fn row(&self, path: &Path) -> Option<MediaFile> {
        self.file_repo
            .find_by_path(&path.to_string_lossy())
            .await
            .unwrap()
    }

    async fn present(&self, path: &Path) -> MediaFile {
        let row = self
            .row(path)
            .await
            .unwrap_or_else(|| panic!("no row at {}", path.display()));
        assert_eq!(row.missing_since, None, "{} is missing", path.display());
        row
    }

    fn probes(&self) -> usize {
        self.prober.calls.load(Ordering::SeqCst)
    }

    fn hashes(&self) -> usize {
        self.hasher.calls.load(Ordering::SeqCst)
    }

    async fn row_count(&self) -> usize {
        self.file_repo.files.lock().unwrap().len()
    }

    async fn move_logs(&self) -> Vec<AdminLog> {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|log| log.message.starts_with("File moved"))
            .collect()
    }
}

/// The pure planners, table-driven: which gone row a moved file relinks
/// to, and where each row's content went across a walk.
#[path = "index_relink_planner_tests.rs"]
mod planner;

/// The scan and the watcher over a real `TempDir` library: renames,
/// moves, swaps and rotations keep each file's row.
#[path = "index_relink_integration_tests.rs"]
mod integration;
