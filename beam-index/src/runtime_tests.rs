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
    /// Every single-library scan, in order.
    library_scans: std::sync::Mutex<Vec<Uuid>>,
    reconciled: std::sync::Mutex<Vec<(Uuid, PathBuf, FsEventKind)>>,
    library_repo: Arc<InMemoryLibraryRepository>,
    /// When set, `scan_all_libraries` fails -- the loop must survive it.
    fail_scans: bool,
    /// When set, `scan_all_libraries` holds until a permit is added, so a
    /// test can act while a full scan is in progress.
    scan_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Full scans started and not yet finished.
    scans_in_flight: AtomicU32,
    /// Single-library scans that started while a full scan was in progress.
    overlapping_library_scans: std::sync::Mutex<Vec<Uuid>>,
    /// When set, each full scan records what this watcher had registered
    /// when the scan started.
    watcher: Option<Arc<InMemoryFsWatcher>>,
    watched_at_scan_start: std::sync::Mutex<Vec<Vec<Uuid>>>,
}

impl RecordingIndexer {
    fn scan_count(&self) -> u32 {
        self.scans.load(Ordering::SeqCst)
    }

    fn reconciled(&self) -> Vec<(Uuid, PathBuf, FsEventKind)> {
        self.reconciled.lock().unwrap().clone()
    }

    fn library_scans(&self) -> Vec<Uuid> {
        self.library_scans.lock().unwrap().clone()
    }

    fn overlapping_library_scans(&self) -> Vec<Uuid> {
        self.overlapping_library_scans.lock().unwrap().clone()
    }

    fn watched_at_scan_start(&self) -> Vec<Vec<Uuid>> {
        self.watched_at_scan_start.lock().unwrap().clone()
    }

    fn store(&self, library: &Library) {
        self.library_repo
            .libraries
            .lock()
            .unwrap()
            .insert(library.id, library.clone());
    }

    fn delete(&self, library_id: Uuid) {
        self.library_repo
            .libraries
            .lock()
            .unwrap()
            .remove(&library_id);
    }
}

#[async_trait::async_trait]
impl BackgroundIndexer for RecordingIndexer {
    async fn scan_all_libraries(&self) -> Result<u32, IndexError> {
        if let Some(watcher) = &self.watcher {
            self.watched_at_scan_start
                .lock()
                .unwrap()
                .push(watcher.watched_libraries());
        }
        self.scans_in_flight.fetch_add(1, Ordering::SeqCst);
        self.scans.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.scan_gate {
            // Returned on drop, so once opened the gate stays open.
            drop(gate.acquire().await.unwrap());
        }
        self.scans_in_flight.fetch_sub(1, Ordering::SeqCst);
        if self.fail_scans {
            return Err(IndexError::LibraryNotFound);
        }
        Ok(0)
    }

    async fn scan_library(&self, library_id: Uuid) -> Result<u32, IndexError> {
        if self.scans_in_flight.load(Ordering::SeqCst) > 0 {
            self.overlapping_library_scans
                .lock()
                .unwrap()
                .push(library_id);
        }
        self.library_scans.lock().unwrap().push(library_id);
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
        watch_poll_interval_secs: 300,
    }
}

/// Yield until `condition` holds, or fail after a generous deadline.
///
/// The watcher calls run on the blocking pool, on another thread, so a fixed
/// number of yields is not enough to be sure one has finished; the deadline
/// only bounds a hang and never orders anything. The `TestClock` never
/// advances on its own, so this cannot mask a missing `advance`.
async fn until(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
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
    until("the startup scan", || indexer.scan_count() == 1).await;
    until("the maintenance loop to sleep", || {
        clock.waiter_count() == 1
    })
    .await;

    assert_eq!(
        indexer.scan_count(),
        1,
        "the startup scan must not wait for the rescan interval, and runs once"
    );

    tasks.periodic_maintenance.abort();
}

#[tokio::test]
async fn the_periodic_rescan_fires_once_per_interval_and_not_before() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), None, clock.clone(), config(3600, 2000));
    until("the startup scan", || indexer.scan_count() == 1).await;
    // The maintenance loop then parks on its first sleep.
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
    until("the startup scan", || indexer.scan_count() == 1).await;
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
    until("the maintenance loop and the poller to sleep", || {
        clock.waiter_count() == 2
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

    // Three sleepers: the maintenance interval, the poll interval and the
    // debounce window.
    until("the debounce window to open", || clock.waiter_count() >= 3).await;
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

    until("the debounce window to open", || clock.waiter_count() >= 3).await;
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

// ── Watch lifecycle and newly polled libraries (issue #186) ─────────────────

#[tokio::test]
async fn deleted_libraries_are_unwatched_on_the_next_cycle() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let kept = library("kept", "/videos/kept");
    let deleted = library("deleted", "/videos/deleted");
    indexer.store(&kept);
    indexer.store(&deleted);

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(60, 2000),
    );
    until("both libraries to be watched", || {
        let mut watched = watcher.watched_libraries();
        watched.sort();
        let mut expected = vec![kept.id, deleted.id];
        expected.sort();
        watched == expected
    })
    .await;

    indexer.delete(deleted.id);
    until("the maintenance loop and the poller to sleep", || {
        clock.waiter_count() == 2
    })
    .await;
    clock.advance(Duration::from_secs(60));

    until("the deleted library to be unwatched", || {
        watcher.watched_libraries() == vec![kept.id]
    })
    .await;

    tasks.abort();
}

#[tokio::test]
async fn a_library_recreated_at_a_deleted_ones_root_is_watched_under_its_new_id() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let original = library("Movies", "/videos/movies");
    indexer.store(&original);

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(60, 2000),
    );
    until("the original to be watched", || {
        watcher.watched_libraries() == vec![original.id]
    })
    .await;

    // Deleted and registered again at the same path between two cycles.
    indexer.delete(original.id);
    let recreated = library("Movies", "/videos/movies");
    indexer.store(&recreated);
    until("the maintenance loop and the poller to sleep", || {
        clock.waiter_count() == 2
    })
    .await;
    clock.advance(Duration::from_secs(60));

    until("only the re-created library to be watched", || {
        watcher.watched_libraries() == vec![recreated.id]
    })
    .await;

    tasks.abort();
}

/// The startup scan waits for the watches, and nothing scans a single library
/// alongside it (issue #186 review, D1).
///
/// A polled library's changes are measured against the snapshot taken when
/// it is registered. Registering it before the startup scan starts means that
/// scan covers everything the snapshot misses, so the library needs no scan of
/// its own; registering it alongside the scan scanned it twice at once.
#[tokio::test]
async fn the_startup_scan_starts_after_the_watches_and_no_library_scan_overlaps_it() {
    use crate::services::watch_status::PollReason;

    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let indexer = Arc::new(RecordingIndexer {
        scan_gate: Some(gate.clone()),
        watcher: Some(watcher.clone()),
        ..Default::default()
    });
    let native = library("local", "/videos/local");
    let polled = library("share", "/mnt/share");
    indexer.store(&native);
    indexer.store(&polled);
    watcher.register_as(polled.id, WatchMode::Polling(PollReason::NetworkFilesystem));

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );
    until("the startup scan to start", || indexer.scan_count() == 1).await;
    // The maintenance task is in the scan and the consumer waits on events, so
    // a sleeper here could only be the poller -- which must wait on the startup
    // scan, not on its interval. The poller was spawned first, so it has parked
    // by now on whichever it waits for.
    assert_eq!(
        clock.waiter_count(),
        0,
        "the poller waits for the startup scan before it starts its interval"
    );

    let mut watched_at_start = indexer.watched_at_scan_start()[0].clone();
    watched_at_start.sort();
    let mut expected = vec![native.id, polled.id];
    expected.sort();
    assert_eq!(
        watched_at_start, expected,
        "every library is registered before the startup scan starts"
    );

    // While the startup scan runs, the native library hits the watch limit and
    // a poll interval passes: the demotion must wait for the scan.
    watcher.demote_on_next_poll(native.id);
    clock.advance(Duration::from_secs(300));
    assert_eq!(
        watcher.poll_count(),
        0,
        "nothing is polled during the startup scan"
    );

    gate.add_permits(1);
    until("both loops to sleep", || clock.waiter_count() == 2).await;
    clock.advance(Duration::from_secs(300));
    until("the demoted library's scan", || {
        indexer.library_scans() == vec![native.id]
    })
    .await;

    assert_eq!(
        indexer.overlapping_library_scans(),
        Vec::<Uuid>::new(),
        "no single-library scan ran alongside the startup scan"
    );
    assert_eq!(
        indexer.library_scans(),
        vec![native.id],
        "the library polled from the start is covered by the startup scan"
    );

    tasks.abort();
}

#[tokio::test]
async fn a_library_registered_as_polled_after_startup_is_scanned_once() {
    use crate::services::watch_status::PollReason;

    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(60, 2000),
    );
    until("the maintenance loop and the poller to sleep", || {
        clock.waiter_count() == 2
    })
    .await;

    let polled = library("share", "/mnt/share");
    indexer.store(&polled);
    watcher.register_as(polled.id, WatchMode::Polling(PollReason::NetworkFilesystem));
    clock.advance(Duration::from_secs(60));
    until("the polled library's scan", || {
        indexer.library_scans() == vec![polled.id]
    })
    .await;
    until("the maintenance loop to re-park", || {
        clock.waiter_count() == 2
    })
    .await;

    // A later cycle registers nothing new, so nothing is scanned again.
    clock.advance(Duration::from_secs(60));
    until("the next periodic rescan", || indexer.scan_count() == 3).await;
    until("the maintenance loop to re-park", || {
        clock.waiter_count() == 2
    })
    .await;
    assert_eq!(
        indexer.library_scans(),
        vec![polled.id],
        "only the polled library, and only once"
    );

    tasks.abort();
}

/// A library the watcher drops because it could not move it to the poller is
/// registered again on the next cycle, not left unwatched until a restart
/// (issue #186 review, D2).
#[tokio::test]
async fn a_library_whose_demotion_failed_is_watched_again_next_cycle() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let lost = library("big", "/videos/big");
    indexer.store(&lost);

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(600, 2000),
    );
    until("both loops to sleep", || clock.waiter_count() == 2).await;
    assert_eq!(watcher.watched_libraries(), vec![lost.id]);

    watcher.fail_demotion_on_next_poll(lost.id);
    clock.advance(Duration::from_secs(300));
    until("the failed demotion to deregister it", || {
        watcher.watched_libraries().is_empty()
    })
    .await;
    until("both loops to sleep", || clock.waiter_count() == 2).await;

    clock.advance(Duration::from_secs(300));
    until("the next maintenance cycle to watch it again", || {
        watcher.watched_libraries() == vec![lost.id]
    })
    .await;

    tasks.abort();
}

#[tokio::test]
async fn a_library_the_poll_moves_to_polling_is_scanned_once() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let demoted = library("big", "/videos/big");
    indexer.store(&demoted);

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );
    until("the library to be watched", || {
        watcher.watched_libraries() == vec![demoted.id]
    })
    .await;
    until("both loops to sleep", || clock.waiter_count() == 2).await;
    assert!(
        indexer.library_scans().is_empty(),
        "a native watch is not scanned"
    );

    watcher.demote_on_next_poll(demoted.id);
    clock.advance(Duration::from_secs(300));
    until("the demoted library's scan", || {
        indexer.library_scans() == vec![demoted.id]
    })
    .await;

    // The next poll demotes nothing, and scans nothing.
    until("the poller to re-park", || clock.waiter_count() == 2).await;
    clock.advance(Duration::from_secs(300));
    until("the second poll", || watcher.poll_count() == 2).await;
    until("the poller to re-park", || clock.waiter_count() == 2).await;
    assert_eq!(indexer.library_scans(), vec![demoted.id]);

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

    /// The admin status reads `enabled` from the status the runtime is handed,
    /// so the production entry point is what has to set it.
    #[tokio::test]
    async fn the_production_spawn_reports_whether_the_watcher_runs() {
        for watch_enabled in [false, true] {
            let status = Arc::new(WatchStatus::new());
            let tasks = spawn_background_indexing(
                service(Arc::new(InMemoryLibraryRepository::default())),
                BackgroundIndexingConfig {
                    watch_enabled,
                    ..config(3600, 2000)
                },
                status.clone(),
            );

            assert_eq!(status.snapshot().enabled, watch_enabled);
            assert_eq!(tasks.watch_poller.is_some(), watch_enabled);
            assert_eq!(tasks.watch_consumer.is_some(), watch_enabled);
            tasks.abort();
        }
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

    /// Files that appeared while a library's native watch was already past
    /// the watch limit -- the new season whose directory could not get a
    /// watch -- are in the poller's first snapshot, so no poll will ever
    /// report them. The demotion itself must bring them into the index.
    #[tokio::test]
    async fn files_created_before_a_demotion_are_indexed_after_it() {
        use beam_domain::repositories::FileRepository;

        let temp = tempfile::tempdir().unwrap();
        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let library = library_repo
            .create(CreateLibrary {
                name: "Shows".to_string(),
                description: None,
                root_path: temp.path().to_path_buf(),
            })
            .await
            .unwrap();
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let indexer: Arc<dyn BackgroundIndexer> = Arc::new(LocalIndexService::new(
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
        let watcher = Arc::new(InMemoryFsWatcher::new());
        let clock = Arc::new(TestClock::new());

        let tasks = spawn_background_indexing_with(
            indexer,
            Some(watcher.clone()),
            clock.clone(),
            config(3600, 2000),
        );
        until("the library to be watched", || {
            watcher.watched_libraries() == vec![library.id]
        })
        .await;
        // The poller sleeps only once the startup scan has finished.
        until("both loops to sleep", || clock.waiter_count() == 2).await;

        // Created while the native watch could not see it.
        let season = temp.path().join("Show").join("Season 2");
        std::fs::create_dir_all(&season).unwrap();
        let episode = season.join("Show.S02E01.mkv");
        std::fs::write(&episode, b"not really an episode").unwrap();

        watcher.demote_on_next_poll(library.id);
        clock.advance(Duration::from_secs(300));

        let path = episode.to_string_lossy().to_string();
        // Bounded like `until`; the scan runs on the poller task.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut indexed = false;
        while std::time::Instant::now() < deadline {
            if file_repo.find_by_path(&path).await.unwrap().is_some() {
                indexed = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(indexed, "the file created before the demotion was indexed");
        tasks.periodic_maintenance.abort();
        if let Some(handle) = &tasks.watch_poller {
            handle.abort();
        }
        if let Some(handle) = &tasks.watch_consumer {
            handle.abort();
        }
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
        tasks.periodic_maintenance.is_finished()
            && tasks
                .watch_consumer
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            && tasks
                .watch_poller
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
    })
    .await;
}

#[tokio::test]
async fn the_watcher_is_polled_once_per_poll_interval_and_not_before() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );
    until("the startup scan to run", || indexer.scan_count() == 1).await;
    // Two sleepers: the maintenance interval and the poll interval.
    until("both loops to sleep", || clock.waiter_count() == 2).await;

    clock.advance(Duration::from_secs(299));
    // Let the poller run: had 299 seconds woken it, it would poll and park
    // again, so waiting for it to be parked orders the check after any poll.
    until("the poller to be parked", || clock.waiter_count() == 2).await;
    assert_eq!(
        watcher.poll_count(),
        0,
        "no poll before the interval elapses"
    );

    clock.advance(Duration::from_secs(1));
    until("the first poll", || watcher.poll_count() == 1).await;
    until("the poller to re-park", || clock.waiter_count() == 2).await;

    clock.advance(Duration::from_secs(300));
    until("the second poll", || watcher.poll_count() == 2).await;
    assert_eq!(
        indexer.scan_count(),
        1,
        "a poll is not a rescan: only the startup scan has run"
    );

    tasks.abort();
}

/// A maintenance task that dies before the startup scan finishes drops the
/// poller's release; the poller must poll on rather than leave the polled
/// libraries unwatched.
#[tokio::test]
async fn the_poller_still_polls_when_maintenance_dies_before_the_startup_scan_finishes() {
    let clock = Arc::new(TestClock::new());
    let watcher = Arc::new(InMemoryFsWatcher::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let indexer = Arc::new(RecordingIndexer {
        scan_gate: Some(gate),
        ..Default::default()
    });

    let tasks = spawn_background_indexing_with(
        indexer.clone(),
        Some(watcher.clone()),
        clock.clone(),
        config(3600, 2000),
    );
    until("the startup scan to start", || indexer.scan_count() == 1).await;
    tasks.periodic_maintenance.abort();
    until("the maintenance task to stop", || {
        tasks.periodic_maintenance.is_finished()
    })
    .await;

    until("the poller to start its interval", || {
        clock.waiter_count() == 1
    })
    .await;
    clock.advance(Duration::from_secs(300));
    until("the first poll", || watcher.poll_count() == 1).await;

    tasks.abort();
}

#[tokio::test]
async fn without_a_watcher_nothing_is_polled() {
    let indexer = Arc::new(RecordingIndexer::default());
    let clock = Arc::new(TestClock::new());

    let tasks =
        spawn_background_indexing_with(indexer.clone(), None, clock.clone(), config(3600, 2000));

    assert!(tasks.watch_poller.is_none());
    assert!(tasks.watch_consumer.is_none());
    tasks.abort();
}
