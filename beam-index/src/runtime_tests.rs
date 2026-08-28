//! Tests for the background indexing tasks.
//!
//! These were previously untestable: the loops took concrete types, built
//! their own `RealClock`, and the spawn handles were dropped on the floor. All
//! three are injected or returned now, so the tests below assert *when* the
//! indexer is called -- the actual behaviour of this module -- with no wall
//! clock involved.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use beam_domain::models::Library;
use beam_domain::repositories::LibraryRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::services::TestClock;

use super::*;
use crate::services::watcher::{FsEvent, InMemoryFsWatcher};

/// Records every call the loops make, so a test can assert on ordering and
/// counts rather than on a fake's internals.
#[derive(Debug, Default)]
struct RecordingIndexer {
    scans: AtomicU32,
    reconciled: std::sync::Mutex<Vec<(Uuid, PathBuf, FsEventKind)>>,
    library_repo: Arc<InMemoryLibraryRepository>,
    /// When set, `scan_all_libraries` fails -- the loop must survive it.
    fail_scans: bool,
}

impl RecordingIndexer {
    fn scan_count(&self) -> u32 {
        self.scans.load(Ordering::SeqCst)
    }

    fn reconciled(&self) -> Vec<(Uuid, PathBuf, FsEventKind)> {
        self.reconciled.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl BackgroundIndexer for RecordingIndexer {
    async fn scan_all_libraries(&self) -> Result<u32, IndexError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if self.fail_scans {
            return Err(IndexError::LibraryNotFound);
        }
        Ok(0)
    }

    async fn reconcile_path(
        &self,
        library_id: Uuid,
        path: PathBuf,
        kind: FsEventKind,
    ) -> Result<(), IndexError> {
        self.reconciled
            .lock()
            .unwrap()
            .push((library_id, path, kind));
        Ok(())
    }

    fn library_repo(&self) -> Arc<dyn LibraryRepository> {
        self.library_repo.clone()
    }
}

fn config(scan_interval_secs: u64, watch_debounce_ms: u64) -> BackgroundIndexingConfig {
    BackgroundIndexingConfig {
        scan_interval_secs,
        watch_enabled: true,
        watch_debounce_ms,
    }
}

/// Yield until `condition` holds, or fail. Cooperative only -- the clock never
/// advances on its own, so this cannot mask a missing `advance`.
async fn until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for: {label}");
}

#[tokio::test]
async fn the_startup_scan_runs_once_without_waiting_for_the_interval() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), None, clock.clone(), config(3600, 2000));
    tasks.startup_scan.await.unwrap();

    assert_eq!(
        indexer.scan_count(),
        1,
        "the startup scan must not wait for the rescan interval"
    );

    tasks.periodic_maintenance.abort();
}

#[tokio::test]
async fn the_periodic_rescan_fires_once_per_interval_and_not_before() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), None, clock.clone(), config(3600, 2000));
    tasks.startup_scan.await.unwrap();
    // The maintenance loop is now parked on its first sleep.
    until("the maintenance loop to sleep", || {
        clock.waiter_count() == 1
    })
    .await;

    assert_eq!(indexer.scan_count(), 1, "only the startup scan so far");

    clock.advance(Duration::from_secs(3599));
    until("the loop to re-park", || clock.waiter_count() == 1).await;
    assert_eq!(
        indexer.scan_count(),
        1,
        "one second short of the interval must not trigger a rescan"
    );

    clock.advance(Duration::from_secs(1));
    until("the first rescan", || indexer.scan_count() == 2).await;

    clock.advance(Duration::from_secs(3600));
    until("the second rescan", || indexer.scan_count() == 3).await;

    tasks.periodic_maintenance.abort();
}

#[tokio::test]
async fn a_failing_rescan_does_not_stop_the_loop() {
    let indexer = Arc::new(RecordingIndexer {
        fail_scans: true,
        ..Default::default()
    });
    let clock = Arc::new(TestClock::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), None, clock.clone(), config(60, 2000));
    tasks.startup_scan.await.unwrap();
    until("the maintenance loop to sleep", || {
        clock.waiter_count() == 1
    })
    .await;

    clock.advance(Duration::from_secs(60));
    until("the first (failing) rescan", || indexer.scan_count() == 2).await;
    clock.advance(Duration::from_secs(60));
    until("a further rescan after the failure", || {
        indexer.scan_count() == 3
    })
    .await;

    tasks.periodic_maintenance.abort();
}

#[tokio::test]
async fn libraries_are_watched_once_each_and_new_ones_picked_up_next_cycle() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());

    let first = library("first", "/videos/first");
    indexer
        .library_repo
        .libraries
        .lock()
        .unwrap()
        .insert(first.id, first.clone());

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(60, 2000),
    );
    until("the first watch registration", || {
        watcher.watched_libraries() == vec![first.id]
    })
    .await;

    // A library created after startup is registered on the next cycle, and the
    // already-watched one is not registered twice.
    let second = library("second", "/videos/second");
    indexer
        .library_repo
        .libraries
        .lock()
        .unwrap()
        .insert(second.id, second.clone());
    until("the maintenance loop to sleep", || {
        clock.waiter_count() >= 1
    })
    .await;
    clock.advance(Duration::from_secs(60));

    until("the second watch registration", || {
        watcher.watched_libraries() == vec![first.id, second.id]
    })
    .await;

    tasks.abort();
}

#[tokio::test]
async fn a_burst_of_events_for_one_path_reconciles_once_after_the_debounce() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let library_id = Uuid::new_v4();

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );

    for kind in [
        FsEventKind::Created,
        FsEventKind::Modified,
        FsEventKind::Modified,
    ] {
        watcher.emit(FsEvent {
            library_id,
            path: PathBuf::from("/videos/first/a.mkv"),
            kind,
        });
    }

    // Two sleepers: the maintenance interval and the debounce window.
    until("the debounce window to open", || clock.waiter_count() >= 2).await;
    assert!(
        indexer.reconciled().is_empty(),
        "nothing is reconciled while the debounce window is still open"
    );

    clock.advance(Duration::from_millis(2000));
    until("the coalesced reconcile", || {
        indexer.reconciled().len() == 1
    })
    .await;

    let reconciled = indexer.reconciled();
    assert_eq!(reconciled[0].0, library_id);
    assert_eq!(reconciled[0].1, PathBuf::from("/videos/first/a.mkv"));
    assert_eq!(
        reconciled[0].2,
        FsEventKind::Modified,
        "the last kind in the burst wins"
    );

    tasks.abort();
}

#[tokio::test]
async fn events_for_different_paths_in_one_burst_each_reconcile() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let library_id = Uuid::new_v4();

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );

    for name in ["a.mkv", "b.mkv"] {
        watcher.emit(FsEvent {
            library_id,
            path: PathBuf::from(format!("/videos/first/{name}")),
            kind: FsEventKind::Created,
        });
    }

    until("the debounce window to open", || clock.waiter_count() >= 2).await;
    clock.advance(Duration::from_millis(2000));
    until("both reconciles", || indexer.reconciled().len() == 2).await;

    let mut paths: Vec<PathBuf> = indexer.reconciled().into_iter().map(|r| r.1).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/videos/first/a.mkv"),
            PathBuf::from("/videos/first/b.mkv"),
        ],
        "coalescing is per path, not per burst"
    );

    tasks.abort();
}

fn library(name: &str, root: &str) -> Library {
    Library {
        id: Uuid::new_v4(),
        name: name.to_string(),
        description: None,
        root_path: PathBuf::from(root),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        last_scan_started_at: None,
        last_scan_finished_at: None,
        last_scan_file_count: None,
    }
}

/// The trait impl that lets the background tasks drive the real indexer.
///
/// The loops above are tested against a recording double; these pin that the
/// production adapter actually forwards to `LocalIndexService` rather than
/// quietly answering for it. Each method is a one-line delegation, which is
/// exactly the shape that survives a rewrite to `Ok(Default::default())`.
mod local_index_service_adapter {
    use std::sync::Arc;

    use beam_domain::models::CreateLibrary;
    use beam_domain::repositories::LibraryRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

    use super::*;
    use crate::services::admin_log::NoOpAdminLogService;
    use crate::services::hash::{HashConfig, LocalHashService};
    use crate::services::index::LocalIndexService;
    use crate::services::media_info::LocalMediaInfoService;
    use crate::services::notification::InMemoryNotificationService;

    fn service(library_repo: Arc<InMemoryLibraryRepository>) -> Arc<LocalIndexService> {
        Arc::new(LocalIndexService::new(
            library_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(LocalHashService::new(HashConfig::default())),
            Arc::new(LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        ))
    }

    #[tokio::test]
    async fn scanning_through_the_trait_reaches_the_real_libraries() {
        // An empty library directory scans to zero *new* files, but only
        // because the scan really ran: an adapter returning `Ok(0)` without
        // calling through would leave the library unscanned forever.
        let temp = tempfile::tempdir().unwrap();
        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        library_repo
            .create(CreateLibrary {
                name: "Movies".to_string(),
                description: None,
                root_path: temp.path().to_path_buf(),
            })
            .await
            .unwrap();

        let indexer: Arc<dyn BackgroundIndexer> = service(library_repo.clone());

        // The repository the watcher refresher lists from is the one that was
        // wired in, not a fresh empty one.
        assert_eq!(indexer.library_repo().find_all().await.unwrap().len(), 1);

        let added = indexer.scan_all_libraries().await.unwrap();
        assert_eq!(added, 0, "an empty directory adds no files");

        // A file appears, and the same call now finds it -- which a stubbed
        // adapter could not do.
        std::fs::write(temp.path().join("Movie.2019.mkv"), b"not really a movie").unwrap();
        let added = indexer.scan_all_libraries().await.unwrap();
        assert_eq!(added, 1, "the scan really walked the library root");
    }

    #[tokio::test]
    async fn reconciling_an_unknown_library_through_the_trait_is_a_no_op_not_an_error() {
        // `reconcile_path` ignores events for libraries that no longer exist;
        // the adapter must forward far enough to reach that decision.
        let indexer: Arc<dyn BackgroundIndexer> =
            service(Arc::new(InMemoryLibraryRepository::default()));

        indexer
            .reconcile_path(
                Uuid::new_v4(),
                PathBuf::from("/videos/gone/a.mkv"),
                FsEventKind::Removed,
            )
            .await
            .expect("an event for a deleted library is ignored, not an error");
    }

    #[tokio::test]
    async fn reconciling_a_created_file_through_the_trait_indexes_it() {
        // A watcher event has to reach the indexer and change something. An
        // adapter that returns `Ok(())` without forwarding leaves the file
        // system watcher inert -- every change waits for the next full rescan.
        use beam_domain::repositories::FileRepository;

        let temp = tempfile::tempdir().unwrap();
        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let library = library_repo
            .create(CreateLibrary {
                name: "Movies".to_string(),
                description: None,
                root_path: temp.path().to_path_buf(),
            })
            .await
            .unwrap();

        let file_repo = Arc::new(InMemoryFileRepository::default());
        let index_service = Arc::new(LocalIndexService::new(
            library_repo,
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(LocalHashService::new(HashConfig::default())),
            Arc::new(LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
        ));
        let indexer: Arc<dyn BackgroundIndexer> = index_service;

        let path = temp.path().join("Movie.2019.mkv");
        std::fs::write(&path, b"not really a movie").unwrap();
        assert!(
            file_repo
                .find_by_path(&path.to_string_lossy())
                .await
                .unwrap()
                .is_none(),
            "nothing is indexed before the event"
        );

        indexer
            .reconcile_path(library.id, path.clone(), FsEventKind::Created)
            .await
            .unwrap();

        assert!(
            file_repo
                .find_by_path(&path.to_string_lossy())
                .await
                .unwrap()
                .is_some(),
            "the created file must be in the index after reconciliation"
        );
    }
}

#[tokio::test]
async fn aborting_stops_every_spawned_task() {
    // The handles exist so a caller can stop the loops; `abort()` returning
    // without touching them would leave the rescan loop running after a
    // shutdown, and every test that spawns one leaking a task.
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), Some(watcher), clock, config(3600, 2000));
    until("the startup scan to run", || indexer.scan_count() == 1).await;

    tasks.abort();

    until("every task to stop", || {
        tasks.startup_scan.is_finished()
            && tasks.periodic_maintenance.is_finished()
            && tasks
                .watch_consumer
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
    })
    .await;
}
