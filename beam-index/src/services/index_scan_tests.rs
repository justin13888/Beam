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
use beam_domain::services::{SequentialIdGenerator, TestClock};
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
    /// The file to append to while it is hashed, if any.
    grow_file: parking_lot::Mutex<Option<PathBuf>>,
    clock: Arc<TestClock>,
    advance_per_hash: parking_lot::Mutex<Duration>,
}

#[async_trait::async_trait]
impl HashService for ScriptedHasher {
    async fn hash_async(&self, _file: std::fs::File) -> std::io::Result<u64> {
        self.gate
            .acquire()
            .await
            .expect("the gate is never closed")
            .forget();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let grow = self.grow_file.lock().clone();
        if let Some(path) = grow {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
            file.write_all(b" and more")?;
            file.set_modified(written_at().into())?;
        }
        self.clock.advance(*self.advance_per_hash.lock());
        Ok(1_000 + call as u64)
    }
}

/// A prober that either reports an hour-long matroska file -- named by the
/// demuxer list FFmpeg reports for one -- or fails, as the test says,
/// counting its calls.
#[derive(Debug, Default)]
struct ScriptedProber {
    calls: AtomicUsize,
    fails: AtomicBool,
}

#[async_trait::async_trait]
impl MediaInfoService for ScriptedProber {
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
            format_name: "matroska,webm".to_string(),
            format_long_name: "Matroska / WebM".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
            chapters: Vec::new(),
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

        let file_repo = Arc::new(InMemoryFileRepository::default());
        // The library counts the files the file store holds, as the SQL
        // store counts the `files` table, so a scan's recorded count is real.
        let library_repo = Arc::new(InMemoryLibraryRepository::with_files(file_repo.clone()));
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
            grow_file: parking_lot::Mutex::new(None),
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
                Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
            )
            .with_clock(clock.clone())
            .with_id_generator(Arc::new(SequentialIdGenerator::new()))
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
    // The job's id is the injected generator's first, and the refused
    // second scan drew none: the library's job is still the first.
    assert_eq!(first.job_id(), SequentialIdGenerator::nth(0));
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

// ─── What a scan records on its library ──────────────────────────────────────

/// A scan stamps its library with when it started and finished, on the
/// injected clock, and with how many of the library's files are present
/// once it is done: a file the scan found gone is kept as missing (#179),
/// and is not counted.
#[tokio::test]
async fn a_scan_records_the_present_file_count_and_its_start_and_finish() {
    let h = Harness::settled().await;
    h.write("Heat (1995).mkv");
    let alien = h.write("Alien (1979).mkv");
    *h.hasher.advance_per_hash.lock() = Duration::from_secs(1);
    let first_start = h.clock.now();

    h.scan().await;

    let stored = h
        .library_repo
        .find_by_id(h.library.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.last_scan_started_at, Some(first_start));
    assert_eq!(
        stored.last_scan_finished_at,
        Some(first_start + chrono::TimeDelta::seconds(2)),
        "finished once both files were hashed"
    );
    assert_eq!(stored.last_scan_file_count, Some(2));

    std::fs::remove_file(&alien).unwrap();
    h.clock.advance(Duration::from_secs(60));
    let second_start = h.clock.now();

    h.scan().await;

    let stored = h
        .library_repo
        .find_by_id(h.library.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        h.row(&alien).await.unwrap().missing_since.is_some(),
        "the gone file is kept as missing"
    );
    assert_eq!(stored.last_scan_started_at, Some(second_start));
    assert_eq!(
        stored.last_scan_finished_at,
        Some(second_start),
        "nothing was hashed: the file left is unchanged"
    );
    assert_eq!(
        stored.last_scan_file_count,
        Some(1),
        "the missing file is not counted"
    );
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
    *h.hasher.grow_file.lock() = Some(path.clone());

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

// ─── Unchanged files ─────────────────────────────────────────────────────────

/// A file on a filesystem with nanosecond mtimes -- ext4, btrfs, xfs -- is
/// recorded at the microsecond precision a row keeps, and a later scan that
/// finds it unchanged does not hash it again (issue #229).
#[tokio::test]
async fn an_unchanged_file_with_a_sub_microsecond_mtime_is_not_rehashed() {
    let h = Harness::settled().await;
    let path = h.write("Heat (1995).mkv");
    let on_disk = written_at() + chrono::TimeDelta::nanoseconds(802_029_432);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(on_disk.into())
        .unwrap();
    h.scan().await;
    assert_eq!(h.hashes(), 1);
    assert_eq!(
        h.row(&path).await.unwrap().mtime,
        Some(written_at() + chrono::TimeDelta::microseconds(802_029)),
        "recorded to the microsecond"
    );

    let progress = h.scan().await;

    assert_eq!(h.hashes(), 1, "an unchanged file is not rehashed");
    assert_eq!(progress.changed, 0);
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

/// A probed file is stored with the registered media type of its container
/// (issue #311), not `video/` and FFmpeg's demuxer list: on its first scan,
/// and again when its content changes and it is probed afresh. FFmpeg's one
/// `matroska,webm` demuxer reads both, so the file name tells them apart.
#[tokio::test]
async fn a_probed_file_is_stored_with_its_containers_registered_mime_type() {
    let h = Harness::settled().await;
    let matroska = h.write("Heat (1995).mkv");
    let webm = h.write("Ronin (1998).webm");

    h.scan().await;

    let first = (h.row(&matroska).await.unwrap(), h.row(&webm).await.unwrap());
    assert_eq!(first.0.mime_type.as_deref(), Some("video/x-matroska"));
    assert_eq!(first.1.mime_type.as_deref(), Some("video/webm"));
    assert_eq!(
        first.1.container_format.as_deref(),
        Some("matroska,webm"),
        "the container keeps FFmpeg's own name"
    );

    std::fs::write(&webm, b"a rewrite that still probes").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&webm)
        .unwrap()
        .set_modified(written_at().into())
        .unwrap();
    let progress = h.scan().await;

    assert_eq!(progress.changed, 1);
    let reprobed = h.row(&webm).await.unwrap();
    assert_ne!(reprobed.hash, first.1.hash, "the change was reconciled");
    assert_eq!(reprobed.mime_type.as_deref(), Some("video/webm"));
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
    let (size_bytes, mtime) = read_fs_meta(path.parent().unwrap(), &path).unwrap();
    h.file_repo
        .create(CreateMediaFile {
            library_id: h.library.id,
            path: path.clone(),
            hash: 0,
            size_bytes,
            mtime,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Unknown,
            classifier_version: 0,
            container_tags: None,
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

/// Whole-library scans run to the end: what a walk adds, changes, marks
/// missing or refuses, and the watcher-driven reconciles beside them.
mod library_scans {
    use super::super::test_harness::*;
    use super::*;

    use crate::probe::metadata::MetadataError;

    use crate::services::admin_log::LocalAdminLogService;
    use crate::services::admin_log::NoOpAdminLogService;
    use crate::services::hash::MockHashService;
    use crate::services::media_info::MockMediaInfoService;
    use crate::services::notification::EventLevel;
    use crate::services::notification::InMemoryNotificationService;
    use beam_domain::models::{CreateLibrary, Library, MediaFile};
    use beam_domain::repositories::AdminLogRepository;
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;

    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;

    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;

    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;

    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;

    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

    use std::path::PathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_scan_library_empty() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty());

        // Nothing was indexed, so there is nothing to lose: a new, empty
        // library scans to completion rather than being refused.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_started_at.is_some());
        assert_eq!(stored.last_scan_file_count, Some(0));
    }

    #[tokio::test]
    async fn test_scan_library_new_video_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"fake video content").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(12345));

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Known);
    }

    /// Only media is indexed (issue #182): a sidecar, a stray text file and
    /// a sample are neither hashed nor probed nor given a row.
    #[tokio::test]
    async fn test_scan_library_indexes_only_media() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let movie = dir.path().join("Movie (2019)");
        std::fs::create_dir_all(&movie).unwrap();
        for name in [
            "notes.txt",
            "Movie.2019.en.srt",
            "movie.nfo",
            "poster.jpg",
            "sample.mkv",
        ] {
            std::fs::write(movie.join(name), b"not media").unwrap();
        }

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0);
        assert!(
            file_repo
                .find_all_by_library(library.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_scan_library_multiple_new_files() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        for name in &["alpha.mkv", "beta.mkv", "gamma.mkv"] {
            std::fs::write(dir.path().join(name), b"fake video").unwrap();
        }

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(3)
            .returning(|_| Ok(99999));

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(3)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 3);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    async fn test_scan_library_changed_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // A real video file on disk (16 bytes).
        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"new content size").unwrap();

        // Seed the DB with the same path but a stale hash/size, so the scan
        // detects the content change and reconciles it.
        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 12345,
            size_bytes: 999,
            mtime: None,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing.clone());

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(99999));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0); // no new files added

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        // The changed file was re-hashed, re-extracted and is healthy again.
        assert_eq!(files[0].status, FileStatus::Known);
        assert_eq!(files[0].size_bytes, 16);
        assert_eq!(files[0].hash, 99999);
    }

    #[tokio::test]
    async fn test_scan_library_removed_file_is_marked_missing_not_deleted() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // A file that is still on disk and unchanged since it was indexed, so
        // the root is not empty and the scan reconciles rather than refuses.
        let kept_path = dir.path().join("kept.mp4");
        std::fs::write(&kept_path, b"still here").unwrap();
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

        // Seed the file repo with a phantom file that doesn't exist on disk
        let phantom = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: dir.path().join("ghost.mp4"),
            hash: 0,
            size_bytes: 1024,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(phantom.id, phantom.clone());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0); // no new files

        // The phantom is hidden from the visible reads but kept, stamped
        // missing, so its id -- and anything keyed on it -- survives (#179).
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        let ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![kept.id]);
        let stored = file_repo
            .find_by_path(&phantom.path.to_string_lossy())
            .await
            .unwrap()
            .expect("a missing file is soft-deleted, not deleted");
        assert!(stored.missing_since.is_some());
    }

    /// The row a previous scan would have written for a file that is on disk
    /// and unchanged, so reconciling it takes the size-and-mtime fast path and
    /// touches neither the hasher nor the prober: hashed, and probed (a row
    /// whose probe never succeeded is probed again on every visit).
    fn indexed_file_matching_disk(library_id: Uuid, path: &Path) -> MediaFile {
        let (size_bytes, mtime) = read_fs_meta(path.parent().unwrap(), path).unwrap();
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: path.to_path_buf(),
            hash: 7,
            size_bytes,
            mtime,
            identity: None,
            mime_type: None,
            duration: Some(Duration::from_secs(60)),
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    /// A row for a file indexed under `root` by an earlier scan and not on
    /// disk now.
    fn indexed_file_under(library_id: Uuid, root: &Path, name: &str) -> MediaFile {
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: root.join(name),
            hash: 42,
            size_bytes: 1024,
            mtime: None,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    /// A scan service over `root` for a library that already has `indexed`
    /// rows, with the admin log and notifications captured.
    struct IndexedLibraryHarness {
        library: Library,
        lib_repo: Arc<InMemoryLibraryRepository>,
        file_repo: Arc<InMemoryFileRepository>,
        notification_svc: Arc<InMemoryNotificationService>,
        admin_log_repo: Arc<InMemoryAdminLogRepository>,
        service: LocalIndexService,
    }

    async fn indexed_library_harness(root: &Path, indexed: &[&str]) -> IndexedLibraryHarness {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let library = lib_repo
            .create(CreateLibrary {
                name: "Mounted Library".to_string(),
                root_path: root.to_path_buf(),
                description: None,
            })
            .await
            .unwrap();
        for name in indexed {
            let row = indexed_file_under(library.id, root, name);
            file_repo.files.lock().unwrap().insert(row.id, row);
        }
        // No expectations: reaching the hasher or the prober would be a bug.
        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );
        IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            notification_svc,
            admin_log_repo,
            service,
        }
    }

    #[tokio::test]
    async fn test_scan_library_empty_root_with_indexed_files_is_refused() {
        // An unmounted volume: the mount point is left behind as an empty
        // directory while the catalogue still holds the library's files.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            notification_svc,
            admin_log_repo,
            service,
        } = indexed_library_harness(root, &["a.mp4", "b.mkv"]).await;

        let err = service
            .scan_library(library.id)
            .await
            .expect_err("an empty root under indexed files must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[root]);

        // Nothing was reconciled away.
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 2, "a refused scan must not delete any row");

        // A refused scan never started, so the library must not read as
        // scanning.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
        assert_eq!(stored.last_scan_finished_at, None);

        // The operator is told, with the root and the count at stake.
        let root_str = root.to_string_lossy();
        let errors: Vec<_> = notification_svc
            .published_events()
            .into_iter()
            .filter(|e| {
                matches!(e.level, EventLevel::Error)
                    && matches!(e.category, EventCategory::LibraryScan)
            })
            .collect();
        assert_eq!(errors.len(), 1, "exactly one error event: {errors:?}");
        assert!(
            errors[0].message.contains(root_str.as_ref()),
            "the notification must name the refused root: {:?}",
            errors[0].message
        );
        assert!(
            errors[0].message.contains("2 are indexed"),
            "the notification must say how many rows were at stake: {:?}",
            errors[0].message
        );

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused scan must write an error-level LibraryScan admin-log entry");
        let details = entry
            .details
            .as_ref()
            .expect("the admin-log entry must carry a details payload");
        assert_eq!(
            details.get("path").and_then(|p| p.as_str()),
            Some(root_str.as_ref())
        );
        assert_eq!(
            details.get("indexed_video_files").and_then(|n| n.as_u64()),
            Some(2)
        );
        assert_eq!(
            details.get("library_id").and_then(|id| id.as_str()),
            Some(library.id.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn test_scan_library_root_with_only_empty_subdirs_is_refused() {
        // A root that is itself a directory of mount points, none of which is
        // mounted: directories, but not one file anywhere beneath them.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("disk1")).unwrap();
        std::fs::create_dir_all(root.join("disk2").join("Movies")).unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["disk1/a.mp4", "disk2/Movies/b.mkv"]).await;

        let err = service
            .scan_library(library.id)
            .await
            .expect_err("a root holding only empty directories must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 2, "a refused scan must not delete any row");
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
    }

    #[tokio::test]
    async fn test_scan_library_root_with_only_sentinel_files_is_refused() {
        // An unmounted volume whose mount point carries the files admins and
        // desktop OSes leave there on purpose: regular files, but not one
        // that Beam can index as media.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".not_mounted"), b"").unwrap();
        std::fs::write(root.join(".DS_Store"), b"\0").unwrap();
        std::fs::create_dir_all(root.join("Movies")).unwrap();
        std::fs::write(root.join("Movies").join("Thumbs.db"), b"\0").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            admin_log_repo,
            service,
            ..
        } = indexed_library_harness(root, &["a.mp4", "Movies/b.mkv"]).await;
        let before: Vec<Uuid> = {
            let mut ids: Vec<Uuid> = file_repo
                .find_all_by_library(library.id)
                .await
                .unwrap()
                .iter()
                .map(|f| f.id)
                .collect();
            ids.sort();
            ids
        };

        let err = service
            .scan_library(library.id)
            .await
            .expect_err("a root holding only non-video files must refuse the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[root]);

        // Every row survives, and the sentinels were not indexed either.
        let mut after: Vec<Uuid> = file_repo
            .find_all_by_library(library.id)
            .await
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        after.sort();
        assert_eq!(after, before, "a refused scan must not touch any row");
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused scan must write an error-level LibraryScan admin-log entry");
        assert_eq!(
            entry
                .details
                .as_ref()
                .and_then(|d| d.get("indexed_video_files"))
                .and_then(|n| n.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn test_scan_library_without_indexed_video_files_is_not_refused() {
        // The guard protects indexed video rows. A library that has only ever
        // held non-video files has none, so a walk finding no video files is
        // believed and a non-video file that went is marked missing as usual.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let kept_path = root.join("notes.nfo");
        std::fs::write(&kept_path, b"still here").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["gone.txt"]).await;
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

        assert_eq!(service.scan_library(library.id).await.unwrap(), 0);

        // Both rows are legacy: sidecars and stray files are no longer
        // indexed (issue #182), so the one still on disk goes missing too.
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "{files:?}");
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_one_file_left_still_reconciles() {
        // The guard is for a walk that found no video files. A root that still
        // holds one is believed: the rows for the files that went are marked
        // missing.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let kept_path = root.join("kept.mp4");
        std::fs::write(&kept_path, b"still here").unwrap();
        let IndexedLibraryHarness {
            library,
            lib_repo,
            file_repo,
            service,
            ..
        } = indexed_library_harness(root, &["gone.mp4"]).await;
        let kept = indexed_file_matching_disk(library.id, &kept_path);
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(kept.id, kept.clone());

        assert_eq!(service.scan_library(library.id).await.unwrap(), 0);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        let ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![kept.id]);
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(stored.last_scan_started_at.is_some());
        assert!(stored.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_invalid_root_path() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());

        // Insert a library whose root_path does not exist on disk. Derived from
        // a `TempDir` rather than a hardcoded `/tmp` name so the test does not
        // rest on an assumption about the host's filesystem: the parent is real
        // and this run owns it, and the child is guaranteed absent.
        let dir = TempDir::new().unwrap();
        let root_path = dir.path().join("beam-nonexistent-root");
        let library = Library {
            id: Uuid::new_v4(),
            name: "Bad Library".to_string(),
            root_path: root_path.clone(),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        };
        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(library.id, library.clone());

        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        let err = result.expect_err("a missing root must fail the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        // The path reaches the operator through the notification and admin
        // log below, never through the error message (NFR-108).
        assert_names_no_path(&err, &[&root_path]);

        // The compensating disclosure, asserted rather than assumed. Taking the
        // path out of the client-facing message is only safe because it reaches
        // the operator here instead, so this is the half of NFR-108's bargain
        // that has to be pinned: without it, the message could stay dutifully
        // path-free while the path reached nobody at all.
        let root = root_path.to_string_lossy();

        let events = notification_svc.published_events();
        let notification = events
            .iter()
            .find(|e| {
                matches!(e.level, EventLevel::Error)
                    && matches!(e.category, EventCategory::LibraryScan)
            })
            .expect("a refused root must publish an error-level LibraryScan event");
        assert!(
            notification.message.contains(root.as_ref()),
            "the notification is where the operator reads the root the scan refused; \
             {:?} does not name {root:?}",
            notification.message
        );

        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let entry = logs
            .iter()
            .find(|l| {
                l.level == AdminLogLevel::Error && l.category == AdminLogCategory::LibraryScan
            })
            .expect("a refused root must write an error-level LibraryScan admin-log entry");
        let details = entry
            .details
            .as_ref()
            .expect("the admin-log entry must carry a details payload");
        assert_eq!(
            details.get("path").and_then(|p| p.as_str()),
            Some(root.as_ref()),
            "the admin-log payload is the operator's other route to the root: {details:?}"
        );
    }

    #[tokio::test]
    async fn test_scan_library_root_is_a_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        // The root exists, but as a regular file: there is nothing to walk.
        let root_path = dir.path().join("movies.mkv");
        std::fs::write(&root_path, b"not a directory").unwrap();
        let library = lib_repo
            .create(CreateLibrary {
                name: "File Root".to_string(),
                root_path: root_path.clone(),
                description: None,
            })
            .await
            .unwrap();

        // No expectations: reaching the hasher or the prober would be a bug.
        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let err = service
            .scan_library(library.id)
            .await
            .expect_err("a root that is not a directory must fail the scan");
        assert!(matches!(err, IndexError::PathNotFound(_)));
        assert_names_no_path(&err, &[&root_path]);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "nothing may be indexed under a file root");

        // A refused scan never started, so the library must not read as
        // scanning with no finish to follow.
        let stored = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert_eq!(stored.last_scan_started_at, None);
    }

    #[tokio::test]
    async fn test_scan_library_media_extraction_failure() {
        // When media-info extraction fails, process_new_file still inserts the file
        // with Unknown status, so added_count is incremented -- and with its
        // real hash, taken before the probe, so the row takes part in
        // duplicate detection rather than carrying the unhashed sentinel.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("corrupt.mp4");
        std::fs::write(&file_path, b"not real video data").unwrap();

        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".to_string())));
        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(31337));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 1);

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
        assert_eq!(
            files[0].hash, 31337,
            "the probe failure keeps the real hash"
        );
        assert_eq!(files[0].classifier_version, 0, "never classified");
    }

    #[tokio::test]
    async fn test_scan_library_real_prober_marks_corrupt_file_unknown() {
        // Same Unknown-on-unprobeable behaviour as above, but exercised through
        // the REAL FFmpeg prober (LocalMediaInfoService) rather than a stub:
        // a corrupt .mp4 makes `from_library_file` return Err, and the scan must
        // absorb it (file inserted as Unknown, scan still Ok) rather than abort.
        let _ = crate::probe::init();

        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("corrupt.mp4");
        std::fs::write(&file_path, b"not a real mp4 container at all").unwrap();

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            // Every file is hashed before it is probed.
            Arc::new(crate::services::hash::LocalHashService::default()),
            Arc::new(crate::services::media_info::LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 1, "scan must succeed despite corrupt file");

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
    }

    #[tokio::test]
    async fn test_scan_library_process_failure_sends_warning() {
        // When process_new_file returns Err (e.g. hash fails), scan_library
        // publishes a warning notification and continues rather than aborting.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("problem.mp4");
        std::fs::write(&file_path, b"video data").unwrap();

        // The hash fails first, so the file is never probed.
        let mock_media_info = MockMediaInfoService::new();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Err(std::io::Error::other("hash io error")));

        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            notification_svc.clone(),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        // Scan should succeed overall; the failing file is not counted
        let result = service.scan_library(library.id).await;
        assert_eq!(result.unwrap(), 0);

        // A warning notification should have been published for the failed file
        let events = notification_svc.published_events();
        assert!(events.iter().any(|e| {
            matches!(e.level, EventLevel::Warning)
                && matches!(e.category, EventCategory::LibraryScan)
        }));

        // Admin log must also have a warning entry mentioning the failed file path
        let logs = admin_log_repo.list(10, 0).await.unwrap();
        let file_path_str = file_path.display().to_string();
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Warning
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains(&file_path_str)
        }));

        // The file must not have been added to the repo
        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_scan_library_updates_timestamps() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        assert!(library.last_scan_started_at.is_none());
        assert!(library.last_scan_finished_at.is_none());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let updated = lib_repo.find_by_id(library.id).await.unwrap().unwrap();
        assert!(updated.last_scan_started_at.is_some());
        assert!(updated.last_scan_finished_at.is_some());
    }

    #[tokio::test]
    async fn test_scan_library_admin_log_and_notifications() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let notification_svc = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let service = LocalIndexService::new(
            lib_repo.clone(),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notification_svc.clone(),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        // At least one Info notification with LibraryScan category whose message names the library
        let events = notification_svc.published_events();
        assert!(events.iter().any(|e| {
            matches!(e.level, EventLevel::Info)
                && matches!(e.category, EventCategory::LibraryScan)
                && e.message.contains("Test Library")
        }));

        // Admin log must have a "scan started" entry
        let logs = admin_log_repo.list(10, 0).await.unwrap();
        assert!(!logs.is_empty());
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Info
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains("scan started")
        }));

        // Admin log must have a "scan completed" entry
        assert!(logs.iter().any(|l| {
            l.level == AdminLogLevel::Info
                && l.category == AdminLogCategory::LibraryScan
                && l.message.contains("scan completed")
        }));
    }

    #[tokio::test]
    async fn test_scan_publishes_correct_event_counts() {
        // Seed: 2 pre-existing DB records, 1 matching file on disk and 1 phantom.
        // Disk: 1 matching file (stays) + 1 phantom (marked missing) + 1 brand-new file (added).
        // Expected: added=1, marked_missing=1 in the admin-log completion entry.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // File A: exists in DB and on disk with the same size and hash →
        // stays unchanged
        let stays_path = dir.path().join("stays.mkv");
        std::fs::write(&stays_path, b"hello").unwrap(); // 5 bytes
        let file_a = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: stays_path.clone(),
            hash: 0,
            size_bytes: 5,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);

        // File B: exists in DB only (phantom, no matching disk file) → will be marked missing
        let phantom_path = dir.path().join("phantom.mkv");
        let file_b = beam_domain::models::MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: phantom_path,
            hash: 0,
            size_bytes: 100,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        };
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        // File C: exists on disk only (not in DB) → added as Unknown (its
        // probe fails)
        let new_path = dir.path().join("new_file.mkv");
        std::fs::write(&new_path, b"new").unwrap();
        // File D: a sample → excluded, never indexed
        std::fs::write(dir.path().join("new_file-sample.mkv"), b"sample").unwrap();
        let mut unchanged_hash = MockHashService::new();
        unchanged_hash.expect_hash_async().returning(|_| Ok(0));
        let mut failing_probe = MockMediaInfoService::new();
        failing_probe
            .expect_get_video_metadata()
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".to_string())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(unchanged_hash),
            Arc::new(failing_probe),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let added = service.scan_library(library.id).await.unwrap();
        assert_eq!(added, 1);

        // Admin log completion entry must record the counts in its JSON details
        let logs = admin_log_repo.list(100, 0).await.unwrap();
        let completion = logs
            .iter()
            .find(|l| l.message.contains("scan completed"))
            .expect("expected a 'scan completed' admin log entry");
        let details = completion
            .details
            .as_ref()
            .expect("completion log has JSON details");
        assert_eq!(details["added"], serde_json::json!(1));
        assert_eq!(details["marked_missing"], serde_json::json!(1));
        assert_eq!(details["restored"], serde_json::json!(0));
        assert_eq!(details["purged"], serde_json::json!(0));
        assert_eq!(details["excluded"], serde_json::json!(1));
    }

    // ─── reconcile, dedup, reconcile_path, scan_all_libraries ───────────────

    #[tokio::test]
    async fn test_reconcile_unchanged_file_skips_rehash() {
        // A file whose size AND mtime match the DB record must not be rehashed.
        // MockHashService::new() has no expectation, so any hash call would panic.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"unchanged content").unwrap();
        // As a row records it: at the precision a repository keeps.
        let (size_bytes, mtime) = read_fs_meta(file_path.parent().unwrap(), &file_path).unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 4242,
            size_bytes,
            mtime,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            // Probed: a row whose probe never succeeded is probed again.
            duration: Some(Duration::from_secs(60)),
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hash, 4242, "hash must not have been rewritten");
        assert_eq!(files[0].status, FileStatus::Known);
    }

    #[tokio::test]
    async fn test_reconcile_same_hash_touches_mtime_only() {
        // Suspected change (mtime differs) but rehash matches → only mtime is
        // refreshed; no ffmpeg call, no status change.
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"same content as hash").unwrap();
        let disk_meta = std::fs::metadata(&file_path).unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 8888,
            size_bytes: disk_meta.len(), // size matches
            mtime: None,                 // stale → suspected
            mime_type: Some("video/mp4".to_string()),
            identity: None,
            duration: Some(Duration::from_secs(60)),
            container_format: Some("mp4".to_string()),
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(8888));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(MockMediaInfoService::new()), // no expectation: ffmpeg must NOT be called
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hash, 8888);
        assert_eq!(files[0].status, FileStatus::Known);
        assert!(files[0].mtime.is_some(), "mtime was refreshed");
    }

    #[tokio::test]
    async fn test_reconcile_changed_file_ffmpeg_failure_marks_changed() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("movie.mp4");
        std::fs::write(&file_path, b"new content").unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 100,
            size_bytes: 999, // wrong size → suspected
            mtime: None,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            // A Known row is a movie's or an episode's file (the `files` CHECK).
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(200)); // differs from existing.hash
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".into())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Changed);
        assert_eq!(files[0].hash, 200);
    }

    /// A file with no movie or episode -- one its path could not classify --
    /// whose content changes and whose re-probe then fails stays `Unknown`:
    /// `Changed` without content is what the `files` CHECK refuses, so the
    /// write would fail and the new hash would never be recorded.
    #[tokio::test]
    async fn test_reconcile_changed_unclassifiable_file_ffmpeg_failure_stays_unknown() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("Show Name/Season 01/Behind the Scenes.mkv");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, b"new content").unwrap();

        let existing = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: file_path.clone(),
            hash: 100,
            size_bytes: 999, // wrong size → suspected
            mtime: None,
            identity: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: Some(Duration::from_secs(60)),
            container_format: Some("matroska".to_string()),
            content: None,
            status: FileStatus::Unknown,
            classifier_version: CLASSIFIER_VERSION,
            container_tags: None,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
            missing_since: None,
        };
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(existing.id, existing);

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(200)); // differs from existing.hash
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Err(MetadataError::UnknownError("ffmpeg failed".into())));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service
            .scan_library(library.id)
            .await
            .expect("the scan does not fail on the file");

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Unknown);
        assert!(files[0].content.is_none(), "{:?}", files[0].content);
        assert_eq!(files[0].hash, 200, "the new content was recorded");
        assert_eq!(files[0].size_bytes, "new content".len() as u64);
    }

    #[tokio::test]
    async fn test_reconcile_path_removed_marks_file_missing() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let ghost_path = dir.path().join("ghost.mp4");
        // The file is intentionally NOT created on disk.

        let phantom = MediaFile {
            id: Uuid::new_v4(),
            library_id: library.id,
            path: ghost_path.clone(),
            hash: 0,
            size_bytes: 10,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: Utc::now(),
            updated_at: Utc::now(),
            missing_since: None,
        };
        let phantom_id = phantom.id;
        file_repo.files.lock().unwrap().insert(phantom.id, phantom);

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service
            .reconcile_path(library.id, ghost_path, FsEventKind::Removed)
            .await
            .unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert!(files.is_empty(), "a removed file is hidden from the index");
        let stored = file_repo
            .find_all_by_library_including_missing(library.id)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1, "a removal never hard-deletes");
        assert_eq!(stored[0].id, phantom_id);
        assert!(stored[0].missing_since.is_some());
    }

    #[tokio::test]
    async fn test_reconcile_path_creates_new_file() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        let file_path = dir.path().join("new.mp4");
        std::fs::write(&file_path, b"fresh video").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash.expect_hash_async().times(1).returning(|_| Ok(42));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(1)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service
            .reconcile_path(library.id, file_path.clone(), FsEventKind::Created)
            .await
            .unwrap();

        let files = file_repo.find_all_by_library(library.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, file_path);
        assert_eq!(files[0].hash, 42);
    }

    #[tokio::test]
    async fn test_reconcile_path_unknown_library_is_noop() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        // No library matches this id; reconcile_path must be a no-op.
        service
            .reconcile_path(
                Uuid::new_v4(),
                PathBuf::from("/nonexistent/path.mp4"),
                FsEventKind::Created,
            )
            .await
            .unwrap();

        assert!(file_repo.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_duplicate_detection_logs() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log_svc = Arc::new(LocalAdminLogService::new(
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        ));
        let dir = TempDir::new().unwrap();
        let library = make_library_in_tempdir(&lib_repo, &dir).await;

        // Two .mp4 files that the mock hash service deliberately hashes to the
        // same value, exercising the dedup-on-create path.
        std::fs::write(dir.path().join("first.mp4"), b"one").unwrap();
        std::fs::write(dir.path().join("second.mp4"), b"two").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(2)
            .returning(|_| Ok(777));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(2)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_svc,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        service.scan_library(library.id).await.unwrap();

        let logs = admin_log_repo.list(100, 0).await.unwrap();
        assert!(
            logs.iter().any(|l| {
                l.level == AdminLogLevel::Info
                    && l.category == AdminLogCategory::LibraryScan
                    && l.message.contains("Duplicate")
            }),
            "an admin log entry must flag the duplicate"
        );
    }

    #[tokio::test]
    async fn test_scan_all_libraries_sums_added_counts() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let _ = make_library_in_tempdir(&lib_repo, &dir_a).await;
        let _ = make_library_in_tempdir(&lib_repo, &dir_b).await;
        std::fs::write(dir_a.path().join("a.mp4"), b"video a").unwrap();
        std::fs::write(dir_b.path().join("b.mp4"), b"video b").unwrap();

        let mut mock_hash = MockHashService::new();
        mock_hash
            .expect_hash_async()
            .times(2)
            .returning(|_| Ok(1234));
        let mut mock_media_info = MockMediaInfoService::new();
        mock_media_info
            .expect_get_video_metadata()
            .times(2)
            .returning(|_| Ok(make_video_metadata()));

        let service = LocalIndexService::new(
            lib_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(mock_hash),
            Arc::new(mock_media_info),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        );

        let total = service
            .scan_all_libraries(ScanTrigger::Periodic)
            .await
            .unwrap();
        assert_eq!(total, 2);
    }
}
