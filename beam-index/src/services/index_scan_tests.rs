//! Scans as coordinated jobs (issue #181): one scan per library at a time,
//! watcher events deferred while one runs, the settle window, retried probes,
//! and the per-item progress events a scan publishes.
//!
//! The filesystem is a real `TempDir`, time the injected `TestClock`. The
//! hasher and prober are scripted doubles below: a test needs to hold a scan
//! mid-file, grow a file while it is hashed, or move the clock per file.

use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::notification::InMemoryNotificationService;
use crate::services::scan::{INTERRUPTED, ScanGuard};
use beam_domain::models::CreateLibrary;
use beam_domain::models::stream::{
    AudioStreamMetadata, CreateMediaStream, StreamMetadata, StreamType,
};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

use crate::probe::metadata::MetadataError;

/// The instant every file here was last written, fixed so the settle window
/// is arithmetic rather than a race with the wall clock.
fn written_at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).expect("valid instant")
}

const SETTLE: Duration = Duration::from_secs(30);

/// A hasher a test scripts: counts its calls, can wait at a gate, can append
/// to the file it is hashing (a copy still running), and can move the clock.
#[derive(Debug)]
struct ScriptedHasher {
    calls: AtomicUsize,
    gate: Arc<tokio::sync::Semaphore>,
    grow_file: AtomicBool,
    clock: Arc<TestClock>,
    advance_per_hash: parking_lot::Mutex<Duration>,
}

#[async_trait::async_trait]
impl HashService for ScriptedHasher {
    fn hash_sync(&self, _path: &Path) -> std::io::Result<u64> {
        unreachable!("the indexer hashes asynchronously")
    }

    async fn hash_async(&self, path: PathBuf) -> std::io::Result<u64> {
        self.gate
            .acquire()
            .await
            .expect("the gate is never closed")
            .forget();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.grow_file.load(Ordering::SeqCst) {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
            file.write_all(b" and more")?;
            file.set_modified(written_at().into())?;
        }
        self.clock.advance(*self.advance_per_hash.lock());
        Ok(1_000 + call as u64)
    }
}

/// A prober that either reports an hour-long matroska file or fails, as the
/// test says, counting its calls.
#[derive(Debug, Default)]
struct ScriptedProber {
    calls: AtomicUsize,
    fails: AtomicBool,
}

#[async_trait::async_trait]
impl MediaInfoService for ScriptedProber {
    async fn get_video_metadata(&self, path: &Path) -> Result<VideoFileMetadata, MetadataError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fails.load(Ordering::SeqCst) {
            return Err(MetadataError::UnknownError(
                "moov atom not found".to_string(),
            ));
        }
        Ok(VideoFileMetadata {
            file_path: path.to_path_buf(),
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
        })
    }
}

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    stream_repo: Arc<InMemoryMediaStreamRepository>,
    notifications: Arc<InMemoryNotificationService>,
    clock: Arc<TestClock>,
    hasher: Arc<ScriptedHasher>,
    prober: Arc<ScriptedProber>,
    service: Arc<LocalIndexService>,
}

impl Harness {
    /// A library with no files, a settle window of [`SETTLE`], and the clock
    /// at `now`.
    async fn at(now: DateTime<Utc>) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let notifications = Arc::new(InMemoryNotificationService::new());
        let clock = Arc::new(TestClock::starting_at(now));
        let library = library_repo
            .create(CreateLibrary {
                name: "Coordinated".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let hasher = Arc::new(ScriptedHasher {
            calls: AtomicUsize::new(0),
            gate: Arc::new(tokio::sync::Semaphore::new(
                tokio::sync::Semaphore::MAX_PERMITS,
            )),
            grow_file: AtomicBool::new(false),
            clock: clock.clone(),
            advance_per_hash: parking_lot::Mutex::new(Duration::ZERO),
        });
        let prober = Arc::new(ScriptedProber::default());
        let service = Arc::new(
            LocalIndexService::new(
                library_repo.clone(),
                file_repo.clone(),
                Arc::new(InMemoryMovieRepository::with_files(file_repo.clone())),
                Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
                stream_repo.clone(),
                hasher.clone(),
                prober.clone(),
                notifications.clone(),
                Arc::new(LocalAdminLogService::new(
                    Arc::new(InMemoryAdminLogRepository::default()) as Arc<dyn AdminLogRepository>,
                )),
            )
            .with_clock(clock.clone())
            .with_settle_window(SETTLE),
        );
        Self {
            _dir: dir,
            root,
            library,
            library_repo,
            file_repo,
            stream_repo,
            notifications,
            clock,
            hasher,
            prober,
            service,
        }
    }

    /// A library whose files have all long settled.
    async fn settled() -> Self {
        Self::at(written_at() + chrono::TimeDelta::hours(1)).await
    }

    /// Write `rel` under the root, last modified at [`written_at`].
    fn write(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(written_at().into())
            .unwrap();
        path
    }

    fn close_gate(&self) {
        let open = self.hasher.gate.available_permits();
        self.hasher.gate.forget_permits(open);
    }

    fn open_gate(&self) {
        self.hasher.gate.add_permits(1_000);
    }

    fn hashes(&self) -> usize {
        self.hasher.calls.load(Ordering::SeqCst)
    }

    fn probes(&self) -> usize {
        self.prober.calls.load(Ordering::SeqCst)
    }

    async fn scan(&self) -> ScanProgress {
        self.service
            .scan_now(self.library.id, ScanTrigger::Manual)
            .await
            .expect("the scan succeeds")
    }

    async fn row(&self, path: &Path) -> Option<MediaFile> {
        self.file_repo
            .find_by_path(&path.to_string_lossy())
            .await
            .unwrap()
    }

    /// Wait -- on the job, not a clock -- until the library's latest job
    /// satisfies `done`.
    async fn wait_for(&self, done: impl FnMut(&Option<ScanJob>) -> bool) -> ScanJob {
        let mut jobs = self.service.subscribe_scan(self.library.id);
        tokio::time::timeout(Duration::from_secs(10), jobs.wait_for(done))
            .await
            .expect("the job reaches the awaited state")
            .expect("the service is alive")
            .clone()
            .expect("a job")
    }

    /// The scan-progress events published so far, in order.
    fn scan_events(&self) -> Vec<ScanEvent> {
        self.notifications
            .published_events()
            .into_iter()
            .filter(|event| event.category == EventCategory::ScanProgress)
            .map(|event| event.scan.expect("a scan-progress event carries its scan"))
            .collect()
    }

    /// Hold the library as a reconcile would.
    fn hold_library(&self) -> ScanGuard {
        self.service
            .scans
            .try_acquire_for_reconcile(self.library.id)
            .expect("the library is free")
    }
}

fn state_is(state: ScanState) -> impl FnMut(&Option<ScanJob>) -> bool {
    move |job| job.as_ref().is_some_and(|job| job.state == state)
}

// ─── One scan per library ────────────────────────────────────────────────────

#[tokio::test]
async fn a_second_scan_is_refused_while_one_is_queued() {
    let h = Harness::settled().await;
    let first = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .expect("the first scan registers");

    let second = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await;

    assert!(matches!(second, Err(IndexError::ScanInProgress)));
    assert_eq!(
        h.service.scan_job(h.library.id).map(|job| job.id),
        Some(first.job_id())
    );
}

/// A scan waits for a library a watcher reconcile holds, reading as queued,
/// and runs once it is free.
#[tokio::test]
async fn a_scan_is_queued_while_the_library_is_held_then_runs() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    let held = h.hold_library();

    let ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();
    let service = h.service.clone();
    let scan = tokio::spawn(async move { service.run_scan(ticket).await });
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        h.service.scan_job(h.library.id).map(|job| job.state),
        Some(ScanState::Queued),
        "the scan waits for the library"
    );
    assert_eq!(h.hashes(), 0, "and touches no file while it waits");

    drop(held);
    let done = h.wait_for(state_is(ScanState::Succeeded)).await;
    assert_eq!(done.progress.added, 1);
    assert!(done.started_at.is_some());
    scan.await.unwrap().unwrap();
}

/// A task that loses its ticket without finishing -- a panic, a runtime
/// shutting down -- leaves a failed job, never one that reads as running.
#[tokio::test]
async fn a_scan_task_that_dies_leaves_its_job_interrupted() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    h.close_gate();
    let ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();
    let service = h.service.clone();
    let scan = tokio::spawn(async move { service.run_scan(ticket).await });
    h.wait_for(state_is(ScanState::Running)).await;

    scan.abort();
    let done = h.wait_for(state_is(ScanState::Failed)).await;

    assert_eq!(done.failure.as_deref(), Some(INTERRUPTED));
}

#[tokio::test]
async fn a_cancelled_scan_stops_and_fails_as_cancelled() {
    let h = Harness::settled().await;
    let first = h.write("Heat (1995).mkv");
    let second = h.write("Ronin (1998).mkv");
    h.close_gate();
    let ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();
    let service = h.service.clone();
    let scan = tokio::spawn(async move { service.run_scan(ticket).await });
    h.wait_for(state_is(ScanState::Running)).await;

    assert!(h.service.scans.cancel(h.library.id, h.clock.now()));
    h.open_gate();

    assert!(matches!(scan.await.unwrap(), Err(IndexError::Cancelled)));
    let job = h.service.scan_job(h.library.id).unwrap();
    assert_eq!(job.state, ScanState::Failed);
    assert_eq!(job.failure.as_deref(), Some(CANCELLED));
    let indexed =
        usize::from(h.row(&first).await.is_some()) + usize::from(h.row(&second).await.is_some());
    assert_eq!(indexed, 1, "the scan stopped after the file it was on");
    assert_eq!(
        h.library_repo
            .find_by_id(h.library.id)
            .await
            .unwrap()
            .unwrap()
            .last_scan_finished_at,
        None,
        "a cancelled scan does not read as finished"
    );
}

/// Wait -- yielding, never sleeping -- until `condition` holds. The deadline
/// only bounds a hang; the `TestClock` never moves on its own, so this cannot
/// stand in for an `advance`.
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

/// Start a scan of the harness's library on a task of its own, held on its
/// first file's hash, and wait until it is running.
async fn a_scan_held_on_its_first_file(
    h: &Harness,
) -> tokio::task::JoinHandle<Result<ScanProgress, IndexError>> {
    h.write("Heat (1995).mkv");
    h.write("Ronin (1998).mkv");
    h.close_gate();
    let ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();
    let service = h.service.clone();
    let scan = tokio::spawn(async move { service.run_scan(ticket).await });
    h.wait_for(state_is(ScanState::Running)).await;
    scan
}

/// Stopping a scan -- what deleting its library does first -- waits until
/// the scan has finished as cancelled, so nothing of it runs once the
/// library's rows go.
#[tokio::test]
async fn stopping_a_scan_waits_until_it_has_failed_as_cancelled() {
    let h = Harness::settled().await;
    let scan = a_scan_held_on_its_first_file(&h).await;

    let service = h.service.clone();
    let library_id = h.library.id;
    let stop = tokio::spawn(async move { service.stop_scan(library_id).await });
    until("the stop to wait on its timeout", || {
        h.clock.waiter_count() == 1
    })
    .await;
    assert!(!stop.is_finished(), "the scan is still on its first file");
    assert_eq!(
        h.service.scan_job(h.library.id).unwrap().state,
        ScanState::Running
    );

    h.open_gate();

    assert!(stop.await.unwrap().in_time, "the scan stopped in time");
    let job = h.service.scan_job(h.library.id).unwrap();
    assert_eq!(
        job.state,
        ScanState::Failed,
        "finished before the stop returned"
    );
    assert_eq!(job.failure.as_deref(), Some(CANCELLED));
    assert!(matches!(scan.await.unwrap(), Err(IndexError::Cancelled)));
}

/// A scan that does not stop within [`SCAN_STOP_TIMEOUT`] of the injected
/// clock is given up on, so a delete is never held for longer; it still
/// fails as cancelled once it reaches the next file.
#[tokio::test]
async fn stopping_a_scan_gives_up_after_the_timeout() {
    let h = Harness::settled().await;
    let scan = a_scan_held_on_its_first_file(&h).await;

    let service = h.service.clone();
    let library_id = h.library.id;
    let stop = tokio::spawn(async move { service.stop_scan(library_id).await });
    until("the stop to wait on its timeout", || {
        h.clock.waiter_count() == 1
    })
    .await;
    h.clock.advance(SCAN_STOP_TIMEOUT - Duration::from_secs(1));
    tokio::task::yield_now().await;
    assert!(!stop.is_finished(), "not yet timed out");
    h.clock.advance(Duration::from_secs(1));

    assert!(!stop.await.unwrap().in_time, "the scan was still running");
    assert_eq!(
        h.service.scan_job(h.library.id).unwrap().state,
        ScanState::Running
    );
    h.open_gate();
    assert!(matches!(scan.await.unwrap(), Err(IndexError::Cancelled)));
}

/// A scan still queued -- here behind a watcher reconcile holding the
/// library -- has nothing to finish, so stopping it returns at once rather
/// than after [`SCAN_STOP_TIMEOUT`], and its task, once it gets the library,
/// does nothing (PR #224 r2).
#[tokio::test]
async fn stopping_a_queued_scan_fails_it_at_once_and_it_never_runs() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    let reconcile = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    let ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();
    let service = h.service.clone();
    let scan = tokio::spawn(async move { service.run_scan(ticket).await });
    tokio::task::yield_now().await;
    assert_eq!(
        h.service.scan_job(h.library.id).unwrap().state,
        ScanState::Queued
    );

    // Returns with the clock never moved -- the `TestClock` does not move on
    // its own -- so it was not held until the timeout.
    assert!(h.service.stop_scan(h.library.id).await.in_time);

    let job = h.service.scan_job(h.library.id).unwrap();
    assert_eq!(job.state, ScanState::Failed);
    assert_eq!(job.failure.as_deref(), Some(CANCELLED));
    drop(reconcile);
    assert!(matches!(scan.await.unwrap(), Err(IndexError::Cancelled)));
    assert_eq!(h.hashes(), 0, "the cancelled scan read no file");
    assert_eq!(
        h.service.scan_job(h.library.id).unwrap(),
        job,
        "and left its job as the stop ended it"
    );
}

/// Once a library's scan is stopped for its delete, nothing starts on it
/// before the delete lands (PR #224 r2): a new scan -- periodic,
/// administrator's or newly polled -- is refused as for a library that is
/// gone, and a watcher event is handed back. A delete that then fails drops
/// the retirement, and the library -- still stored -- is scanned and
/// reconciled again.
#[tokio::test]
async fn a_library_whose_scan_was_stopped_is_not_scanned_or_reconciled_again() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");

    let stopped = h.service.stop_scan(h.library.id).await;
    assert!(stopped.in_time);

    assert!(matches!(
        h.service
            .begin_scan(h.library.id, ScanTrigger::Periodic)
            .await,
        Err(IndexError::LibraryNotFound)
    ));
    assert_eq!(
        h.service
            .scan_all_libraries(ScanTrigger::Periodic)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        h.service
            .reconcile_path(h.library.id, path.clone(), FsEventKind::Created)
            .await,
        Ok(ReconcileOutcome::Deferred { .. })
    ));
    assert_eq!(h.hashes(), 0);
    assert!(h.row(&path).await.is_none(), "nothing was indexed");

    // The delete failed.
    drop(stopped);

    assert_eq!(
        h.service
            .reconcile_path(h.library.id, path.clone(), FsEventKind::Created)
            .await
            .unwrap(),
        ReconcileOutcome::Done
    );
    assert!(h.row(&path).await.is_some(), "the event was reconciled");
    assert!(
        h.service
            .begin_scan(h.library.id, ScanTrigger::Periodic)
            .await
            .is_ok(),
        "and a scan registers"
    );
}

/// With no scan queued or running there is nothing to wait for.
#[tokio::test]
async fn stopping_an_idle_library_returns_at_once() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    h.scan().await;

    assert!(h.service.stop_scan(h.library.id).await.in_time);
    assert_eq!(h.clock.waiter_count(), 0, "it never waited");
    assert_eq!(
        h.service.scan_job(h.library.id).unwrap().state,
        ScanState::Succeeded,
        "a finished job is left as it was"
    );
}

/// A deleted library's slot goes: its latest job is no longer read back.
#[tokio::test]
async fn a_forgotten_library_has_no_latest_job() {
    let h = Harness::settled().await;
    h.scan().await;
    assert!(h.service.scan_job(h.library.id).is_some());

    h.service.forget_library(h.library.id);

    assert_eq!(h.service.scan_job(h.library.id), None);
}

/// The backstop rescan of every library leaves a library an administrator's
/// scan holds to that scan.
#[tokio::test]
async fn a_rescan_of_every_library_skips_one_already_being_scanned() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    let other_root = h._dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = h
        .library_repo
        .create(CreateLibrary {
            name: "Other".to_string(),
            root_path: other_root,
            description: None,
        })
        .await
        .unwrap();
    let manual = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let held = h.service.scan_job(h.library.id).unwrap();
    assert_eq!(held.id, manual.job_id(), "the busy library kept its job");
    assert_eq!(held.state, ScanState::Queued);
    assert_eq!(h.hashes(), 0, "and was not scanned alongside it");
    let scanned = h.service.scan_job(other.id).unwrap();
    assert_eq!(scanned.trigger, ScanTrigger::Periodic);
    assert_eq!(scanned.state, ScanState::Succeeded);
}

// ─── Watcher events while a scan runs ────────────────────────────────────────

/// A watcher event for a library being scanned is handed back untouched: the
/// scan's snapshot of the library's rows -- `missing_since` stamps included,
/// which decide what it purges -- stays true while it runs (issue #209).
#[tokio::test]
async fn a_watcher_event_is_deferred_while_a_scan_holds_the_library() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.scan().await;
    std::fs::remove_file(&path).unwrap();
    let _ticket = h
        .service
        .begin_scan(h.library.id, ScanTrigger::Manual)
        .await
        .unwrap();

    let outcome = h
        .service
        .reconcile_path(h.library.id, path.clone(), FsEventKind::Removed)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        ReconcileOutcome::Deferred {
            retry_after: LIBRARY_BUSY_RETRY
        }
    );
    assert_eq!(
        h.row(&path).await.unwrap().missing_since,
        None,
        "the deferred event wrote nothing"
    );
}

#[tokio::test]
async fn a_watcher_event_reconciles_once_the_scan_has_finished() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.scan().await;
    std::fs::remove_file(&path).unwrap();

    let outcome = h
        .service
        .reconcile_path(h.library.id, path.clone(), FsEventKind::Removed)
        .await
        .unwrap();

    assert_eq!(outcome, ReconcileOutcome::Done);
    assert!(h.row(&path).await.unwrap().missing_since.is_some());
}

// ─── The settle window ───────────────────────────────────────────────────────

/// A file written moments ago -- a copy still running -- is neither hashed nor
/// probed nor written; the scan counts it deferred, and a later scan indexes
/// it once it has settled.
#[tokio::test]
async fn a_file_still_being_written_is_deferred_until_it_settles() {
    let h = Harness::at(written_at() + chrono::TimeDelta::seconds(10)).await;
    let path = h.write("Heat (1995).mkv");

    let first = h.scan().await;

    assert_eq!(first.deferred, 1);
    assert_eq!(first.added, 0);
    assert_eq!(h.hashes(), 0, "a partial file is never hashed");
    assert_eq!(h.probes(), 0);
    assert!(h.row(&path).await.is_none());

    h.clock.advance(Duration::from_secs(20));
    let second = h.scan().await;

    assert_eq!(second.added, 1);
    assert_eq!(h.hashes(), 1, "hashed once, when it had settled");
    assert!(h.row(&path).await.is_some());
}

/// A watcher event for a file still being written comes back for the rest of
/// the window.
#[tokio::test]
async fn a_watcher_event_for_an_unsettled_file_is_deferred_for_the_rest_of_the_window() {
    let h = Harness::at(written_at() + chrono::TimeDelta::seconds(10)).await;
    let path = h.write("Heat (1995).mkv");

    let outcome = h
        .service
        .reconcile_path(h.library.id, path.clone(), FsEventKind::Created)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        ReconcileOutcome::Deferred {
            retry_after: Duration::from_secs(20)
        }
    );
    assert_eq!(h.hashes(), 0);
    assert!(h.row(&path).await.is_none());
}

/// A file that grows while it is hashed describes no whole file: nothing is
/// written for it, and it is deferred.
#[tokio::test]
async fn a_file_that_changes_while_it_is_hashed_is_deferred() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.hasher.grow_file.store(true, Ordering::SeqCst);

    let progress = h.scan().await;

    assert_eq!(progress.deferred, 1);
    assert_eq!(progress.added, 0);
    assert_eq!(
        h.probes(),
        0,
        "a file that moved under the hash is not probed"
    );
    assert!(h.row(&path).await.is_none());
}

/// An indexed file rewritten moments ago keeps its row as it was until the
/// rewrite has settled.
#[tokio::test]
async fn a_changed_file_still_being_written_keeps_its_row_until_it_settles() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&path).await.unwrap();
    std::fs::write(&path, b"a longer rewrite of the file").unwrap();
    let rewritten = h.clock.now() - chrono::TimeDelta::seconds(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(rewritten.into())
        .unwrap();

    let progress = h.scan().await;

    assert_eq!(progress.deferred, 1);
    assert_eq!(h.hashes(), 1, "only the first scan hashed it");
    let after = h.row(&path).await.unwrap();
    assert_eq!(after.size_bytes, before.size_bytes);
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.missing_since, None, "a deferred file is not missing");
}

// ─── Probes that failed ──────────────────────────────────────────────────────

/// A file whose first probe failed keeps its real hash, and is classified the
/// first time a later scan probes it successfully -- without being rehashed.
#[tokio::test]
async fn a_file_whose_probe_failed_is_classified_once_a_later_probe_succeeds() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.prober.fails.store(true, Ordering::SeqCst);
    h.scan().await;
    let unprobed = h.row(&path).await.unwrap();
    assert!(unprobed.content.is_none());
    assert_eq!(unprobed.status, FileStatus::Unknown);
    assert_eq!(unprobed.classifier_version, 0);
    assert_ne!(unprobed.hash, 0, "hashed before the probe failed");

    h.prober.fails.store(false, Ordering::SeqCst);
    let progress = h.scan().await;

    let probed = h.row(&path).await.unwrap();
    assert!(
        matches!(probed.content, Some(MediaFileContent::Movie { .. })),
        "{:?}",
        probed.content
    );
    assert_eq!(probed.status, FileStatus::Known);
    assert_eq!(probed.classifier_version, CLASSIFIER_VERSION);
    assert_eq!(probed.duration, Some(Duration::from_secs(60 * 60)));
    assert_eq!(probed.id, unprobed.id, "the same row");
    assert_eq!(probed.hash, unprobed.hash);
    assert_eq!(h.hashes(), 1, "an unchanged file is not rehashed");
    assert_eq!(progress.changed, 1);
}

/// A file that still does not probe is tried on every visit, and its row is
/// left exactly as it was each time.
#[tokio::test]
async fn a_file_that_still_does_not_probe_is_retried_every_visit_and_left_alone() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.prober.fails.store(true, Ordering::SeqCst);
    h.scan().await;
    let before = h.row(&path).await.unwrap();

    let progress = h.scan().await;

    assert_eq!(h.probes(), 2, "probed again on the second visit");
    assert_eq!(h.hashes(), 1, "but not rehashed");
    assert_eq!(progress.unchanged, 1);
    let after = h.row(&path).await.unwrap();
    assert_eq!(after.status, FileStatus::Unknown);
    assert_eq!(after.updated_at, before.updated_at, "nothing was written");
}

/// A probed file whose content changes and whose probe then fails loses the
/// old content's probe results and streams, so it reads as unprobed: the next
/// scan probes it again -- without rehashing it -- and restores it as `Known`.
#[tokio::test]
async fn a_changed_file_whose_probe_failed_is_probed_again_until_one_succeeds() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    h.scan().await;
    let probed = h.row(&path).await.unwrap();
    assert_eq!(probed.status, FileStatus::Known);
    h.stream_repo
        .insert_streams(vec![CreateMediaStream {
            file_id: probed.id,
            index: 0,
            stream_type: StreamType::Audio,
            codec: "aac".to_string(),
            metadata: StreamMetadata::Audio(AudioStreamMetadata {
                language: None,
                title: None,
                channels: 2,
                sample_rate: 48_000,
                channel_layout: None,
                bit_rate: None,
                is_default: true,
                is_forced: false,
            }),
        }])
        .await
        .unwrap();
    std::fs::write(&path, b"a rewrite whose probe fails").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(written_at().into())
        .unwrap();
    h.prober.fails.store(true, Ordering::SeqCst);

    let progress = h.scan().await;

    assert_eq!(progress.changed, 1);
    let failed = h.row(&path).await.unwrap();
    assert_eq!(failed.status, FileStatus::Changed);
    assert_eq!(failed.content, probed.content, "it keeps its title");
    assert_ne!(failed.hash, probed.hash, "the new content's hash is kept");
    assert_eq!(
        (failed.duration, failed.mime_type, failed.container_format),
        (None, None, None),
        "the old content's probe results are gone"
    );
    assert!(
        h.stream_repo
            .find_by_file_id(probed.id)
            .await
            .unwrap()
            .is_empty(),
        "and so are its streams"
    );

    h.prober.fails.store(false, Ordering::SeqCst);
    let progress = h.scan().await;

    assert_eq!(h.probes(), 3, "probed again on the next visit");
    assert_eq!(h.hashes(), 2, "but not rehashed");
    assert_eq!(progress.changed, 1);
    let restored = h.row(&path).await.unwrap();
    assert_eq!(restored.status, FileStatus::Known);
    assert_eq!(restored.content, probed.content);
    assert_eq!(restored.duration, Some(Duration::from_secs(60 * 60)));
    assert_eq!(restored.hash, failed.hash);
}

/// A row an older build stored unhashed after a failed probe is hashed when
/// it is probed again, so it takes part in duplicate detection.
#[tokio::test]
async fn an_unhashed_row_is_hashed_when_it_is_probed_again() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    let (size_bytes, mtime) = read_fs_meta(&path).unwrap();
    h.file_repo
        .create(CreateMediaFile {
            library_id: h.library.id,
            path: path.clone(),
            hash: 0,
            size_bytes,
            mtime,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Unknown,
            classifier_version: 0,
        })
        .await
        .unwrap();

    h.scan().await;

    let row = h.row(&path).await.unwrap();
    assert_ne!(row.hash, 0);
    assert_eq!(h.hashes(), 1);
    assert!(matches!(row.content, Some(MediaFileContent::Movie { .. })));
}

// ─── Progress events (FR-208) ────────────────────────────────────────────────

/// Every event of a scan names its job; it starts, reports progress, and
/// completes, and with a clock that does not move, per-item progress is let
/// through once.
#[tokio::test]
async fn a_scan_publishes_started_progress_and_completed_for_its_job() {
    let h = Harness::settled().await;
    for name in ["Heat (1995).mkv", "Ronin (1998).mkv", "Thief (1981).mkv"] {
        h.write(name);
    }

    let progress = h.scan().await;

    let job = h.service.scan_job(h.library.id).unwrap();
    let events = h.scan_events();
    let phases: Vec<ScanPhase> = events.iter().map(|event| event.phase).collect();
    assert_eq!(
        phases,
        vec![
            ScanPhase::Started,
            ScanPhase::Progress,
            ScanPhase::Completed
        ],
        "the clock stood still: one progress event, the first"
    );
    assert!(events.iter().all(|event| event.job_id == job.id));
    assert_eq!(events[1].progress.processed, 1);
    assert_eq!(events[1].progress.total, Some(3));
    assert_eq!(events[2].progress, progress);
    assert_eq!(job.progress, progress, "the job holds every file's count");
    assert_eq!(progress.processed, 3);
}

/// Per-item progress goes out at most once a second of the monotonic clock.
#[tokio::test]
async fn progress_events_are_throttled_to_one_a_second() {
    let h = Harness::settled().await;
    for name in [
        "A (2001).mkv",
        "B (2002).mkv",
        "C (2003).mkv",
        "D (2004).mkv",
    ] {
        h.write(name);
    }
    *h.hasher.advance_per_hash.lock() = Duration::from_millis(600);

    h.scan().await;

    // Files finish at 0.6 s, 1.2 s, 1.8 s and 2.4 s: the first goes out, the
    // second is 0.6 s after it, the third 1.2 s, the fourth 0.6 s again.
    let processed: Vec<u64> = h
        .scan_events()
        .into_iter()
        .filter(|event| event.phase == ScanPhase::Progress)
        .map(|event| event.progress.processed)
        .collect();
    assert_eq!(processed, vec![1, 3]);
}

#[tokio::test]
async fn a_failed_scan_publishes_failed_with_what_it_had_done() {
    let h = Harness::settled().await;
    // A root with no video files under a library with indexed ones is refused.
    let path = h.write("Heat (1995).mkv");
    h.scan().await;
    std::fs::remove_file(&path).unwrap();

    let refused = h.service.scan_now(h.library.id, ScanTrigger::Manual).await;

    assert!(matches!(refused, Err(IndexError::PathNotFound(_))));
    let job = h.service.scan_job(h.library.id).unwrap();
    assert_eq!(job.state, ScanState::Failed);
    let failure = job.failure.expect("a failed job says why");
    assert!(
        !failure.contains('/'),
        "the failure names no path: {failure}"
    );
    let last = h.scan_events().pop().expect("events were published");
    assert_eq!(last.phase, ScanPhase::Failed);
    assert_eq!(last.job_id, job.id);
}
