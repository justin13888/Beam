//! Scan jobs, and the coordination that serialises every scan of a library
//! (issue #181).
//!
//! Four things scan or partly reconcile a library: the startup scan, the
//! periodic rescan, the administrator's scan and the watcher's per-path
//! reconcile. They share one [`ScanCoordinator`]:
//!
//! * **A lock per library.** A scan holds it for its whole run; a watcher
//!   reconcile takes it without waiting and, if it cannot, defers its event
//!   (see [`crate::services::index::ReconcileOutcome`]). Two tasks can never
//!   reconcile one library at once, so they can neither insert one path
//!   twice nor purge a row on a `missing_since` stamp a concurrent restore
//!   has since replaced.
//! * **A catalog gate.** Every scan and reconcile holds it shared while it
//!   classifies files; the identity-key passes (backfill and re-derivation,
//!   issue #182) hold it exclusively. No file is classified -- no title
//!   found or created by its key -- while the passes move keys, so a pass
//!   never finds a key taken between reading it free and writing it.
//! * **A job per scan.** A scan is registered as a [`ScanJob`] before it
//!   runs, so a caller can be told one is already queued or running, and can
//!   follow it. The latest job of each library is kept in memory only: it is
//!   what an administrator watches, not a history, and a restart forgets it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
    watch,
};
use uuid::Uuid;

use beam_domain::services::Clock;

/// What started a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanTrigger {
    /// An administrator asked for it.
    Manual,
    /// The scan of every library at process start.
    Startup,
    /// The periodic backstop rescan of every library.
    Periodic,
    /// A library that has just started being polled rather than watched.
    NewlyPolled,
}

/// Where a scan job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    /// Registered, waiting for the library's lock.
    Queued,
    /// Holding the lock and walking the library.
    Running,
    /// Finished; its progress is final.
    Succeeded,
    /// Stopped by an error, a cancellation, or the process losing the task.
    Failed,
}

impl ScanState {
    /// Whether a job in this state still holds, or waits for, the library.
    pub fn is_active(self) -> bool {
        match self {
            ScanState::Queued | ScanState::Running => true,
            ScanState::Succeeded | ScanState::Failed => false,
        }
    }
}

/// How far a scan has got, counted per file (FR-208).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// Files the walk found to index; `None` until the walk has finished.
    pub total: Option<u64>,
    /// Files handled so far, whatever the outcome.
    pub processed: u64,
    /// Files indexed for the first time.
    pub added: u64,
    /// Indexed files whose content changed, or whose probe succeeded at last.
    pub changed: u64,
    /// Indexed files found as they were.
    pub unchanged: u64,
    /// Files still being written, left for a later visit (the settle window).
    pub deferred: u64,
    /// Files that could not be processed.
    pub failed: u64,
    /// Rows the walk did not see, newly marked missing.
    pub marked_missing: u64,
    /// Missing rows whose path came back.
    pub restored: u64,
    /// Rows missing for the whole grace period, removed.
    pub purged: u64,
}

/// One scan of one library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanJob {
    pub id: Uuid,
    pub library_id: Uuid,
    pub trigger: ScanTrigger,
    pub state: ScanState,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress: ScanProgress,
    /// Why a failed job failed. Never names a filesystem path (NFR-108): it
    /// reaches an administrator's browser.
    pub failure: Option<String>,
}

/// Which moment of a scan a [`ScanEvent`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    Started,
    Progress,
    Completed,
    Failed,
}

/// The structured part of a scan-progress event (FR-208).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanEvent {
    pub job_id: Uuid,
    pub phase: ScanPhase,
    pub progress: ScanProgress,
}

/// A scan is already queued or running for the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanInProgress;

/// The failure text of a job whose task went away without finishing it --
/// a panic, or a runtime shutting down.
pub const INTERRUPTED: &str = "interrupted";

/// The failure text of a cancelled job.
pub const CANCELLED: &str = "cancelled";

/// Everything the coordinator keeps for one library.
#[derive(Debug)]
struct LibrarySlot {
    lock: Arc<AsyncMutex<()>>,
    job: watch::Sender<Option<ScanJob>>,
    /// The cancellation flag of the current job; replaced when a job
    /// registers, so cancelling one job never reaches the next.
    cancel: parking_lot::Mutex<Arc<AtomicBool>>,
}

impl LibrarySlot {
    fn new() -> Self {
        let (job, _) = watch::channel(None);
        Self {
            lock: Arc::new(AsyncMutex::new(())),
            job,
            cancel: parking_lot::Mutex::new(Arc::new(AtomicBool::new(false))),
        }
    }
}

/// The per-library locks, the catalog gate and the latest job of each
/// library. See the module documentation.
#[derive(Debug, Default)]
pub struct ScanCoordinator {
    libraries: parking_lot::Mutex<HashMap<Uuid, Arc<LibrarySlot>>>,
    catalog: Arc<RwLock<()>>,
}

/// What a scan holds while it runs: its library's lock, and the catalog gate
/// shared. Dropping it releases both.
#[derive(Debug)]
pub struct ScanGuard {
    _library: OwnedMutexGuard<()>,
    _catalog: OwnedRwLockReadGuard<()>,
}

/// The catalog gate held exclusively, by the identity passes.
#[derive(Debug)]
pub struct CatalogExclusive {
    _catalog: OwnedRwLockWriteGuard<()>,
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, library_id: Uuid) -> Arc<LibrarySlot> {
        self.libraries
            .lock()
            .entry(library_id)
            .or_insert_with(|| Arc::new(LibrarySlot::new()))
            .clone()
    }

    fn existing_slot(&self, library_id: Uuid) -> Option<Arc<LibrarySlot>> {
        self.libraries.lock().get(&library_id).cloned()
    }

    /// Register `job` as its library's current job, unless one is already
    /// queued or running. The check and the registration are one step: two
    /// callers racing for one library cannot both succeed.
    pub fn register(
        &self,
        job: ScanJob,
        clock: Arc<dyn Clock>,
    ) -> Result<ScanTicket, ScanInProgress> {
        let library_id = job.library_id;
        let slot = self.slot(library_id);
        let job_id = job.id;
        let mut incoming = Some(job);
        let registered = slot.job.send_if_modified(|current| {
            if current.as_ref().is_some_and(|job| job.state.is_active()) {
                return false;
            }
            *current = incoming.take();
            true
        });
        if !registered {
            return Err(ScanInProgress);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *slot.cancel.lock() = cancel.clone();
        Ok(ScanTicket {
            slot,
            job_id,
            library_id,
            cancel,
            clock,
        })
    }

    /// The latest job of `library_id`, finished or not.
    pub fn job(&self, library_id: Uuid) -> Option<ScanJob> {
        self.existing_slot(library_id)
            .and_then(|slot| slot.job.borrow().clone())
    }

    /// Follow `library_id`'s jobs: the receiver sees every change to the
    /// latest one, and each job that replaces it.
    pub fn subscribe(&self, library_id: Uuid) -> watch::Receiver<Option<ScanJob>> {
        self.slot(library_id).job.subscribe()
    }

    /// Ask `library_id`'s active job to stop. The scan checks between files,
    /// so it stops after the file it is on; it then fails as
    /// [`CANCELLED`]. Returns whether there was an active job to ask.
    pub fn cancel(&self, library_id: Uuid) -> bool {
        let Some(slot) = self.existing_slot(library_id) else {
            return false;
        };
        let active = slot
            .job
            .borrow()
            .as_ref()
            .is_some_and(|job| job.state.is_active());
        if active {
            slot.cancel.lock().store(true, Ordering::SeqCst);
        }
        active
    }

    /// Take `library_id`'s lock and the catalog gate for a scan, waiting for
    /// both.
    pub async fn acquire_for_scan(&self, library_id: Uuid) -> ScanGuard {
        let lock = self.slot(library_id).lock.clone();
        let library = lock.lock_owned().await;
        let catalog = self.catalog.clone().read_owned().await;
        ScanGuard {
            _library: library,
            _catalog: catalog,
        }
    }

    /// Take `library_id`'s lock and the catalog gate for a watcher reconcile
    /// without waiting: `None` when a scan job is registered for the library,
    /// or either is held. A watcher that waited would stall every other
    /// library's events behind one scan.
    pub fn try_acquire_for_reconcile(&self, library_id: Uuid) -> Option<ScanGuard> {
        let slot = self.slot(library_id);
        if slot
            .job
            .borrow()
            .as_ref()
            .is_some_and(|job| job.state.is_active())
        {
            return None;
        }
        let library = slot.lock.clone().try_lock_owned().ok()?;
        let catalog = self.catalog.clone().try_read_owned().ok()?;
        Some(ScanGuard {
            _library: library,
            _catalog: catalog,
        })
    }

    /// Hold the catalog gate exclusively, waiting for every scan and
    /// reconcile holding it to finish.
    pub async fn exclusive_catalog(&self) -> CatalogExclusive {
        CatalogExclusive {
            _catalog: self.catalog.clone().write_owned().await,
        }
    }

    /// [`Self::exclusive_catalog`] without waiting.
    pub fn try_exclusive_catalog(&self) -> Option<CatalogExclusive> {
        self.catalog
            .clone()
            .try_write_owned()
            .ok()
            .map(|catalog| CatalogExclusive { _catalog: catalog })
    }
}

/// A registered scan job, and the right to run it.
///
/// Whoever holds the ticket updates the job. Dropping a ticket whose job is
/// still queued or running -- its task panicked, or was dropped unfinished --
/// fails the job as [`INTERRUPTED`], so a job never reads as running forever.
pub struct ScanTicket {
    slot: Arc<LibrarySlot>,
    job_id: Uuid,
    library_id: Uuid,
    cancel: Arc<AtomicBool>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ScanTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanTicket")
            .field("job_id", &self.job_id)
            .finish_non_exhaustive()
    }
}

impl ScanTicket {
    /// The job as it stands.
    pub fn job(&self) -> ScanJob {
        self.slot
            .job
            .borrow()
            .as_ref()
            .filter(|job| job.id == self.job_id)
            .cloned()
            .expect("a ticket's job stays its library's current job until the ticket is dropped")
    }

    pub fn job_id(&self) -> Uuid {
        self.job_id
    }

    pub fn library_id(&self) -> Uuid {
        self.library_id
    }

    /// Whether the job has been asked to stop.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn update(&self, change: impl FnOnce(&mut ScanJob)) {
        self.slot.job.send_if_modified(|current| match current {
            Some(job) if job.id == self.job_id => {
                change(job);
                true
            }
            _ => false,
        });
    }

    /// The job has its lock and is running.
    pub fn start(&self) {
        let now = self.clock.now();
        self.update(|job| {
            job.state = ScanState::Running;
            job.started_at = Some(now);
        });
    }

    /// Record the scan's progress so far.
    pub fn record(&self, progress: ScanProgress) {
        self.update(|job| job.progress = progress);
    }

    /// The scan finished.
    pub fn succeed(&self, progress: ScanProgress) {
        let now = self.clock.now();
        self.update(|job| {
            job.state = ScanState::Succeeded;
            job.finished_at = Some(now);
            job.progress = progress;
        });
    }

    /// The scan stopped. `failure` must name no filesystem path.
    pub fn fail(&self, failure: impl Into<String>) {
        let now = self.clock.now();
        let failure = failure.into();
        self.update(|job| {
            job.state = ScanState::Failed;
            job.finished_at = Some(now);
            job.failure = Some(failure);
        });
    }
}

impl Drop for ScanTicket {
    fn drop(&mut self) {
        let now = self.clock.now();
        self.update(|job| {
            if job.state.is_active() {
                job.state = ScanState::Failed;
                job.finished_at = Some(now);
                job.failure = Some(INTERRUPTED.to_string());
            }
        });
    }
}

/// Lets a scan's progress events through at most once per `interval` of the
/// monotonic clock. The first is always let through.
#[derive(Debug)]
pub(crate) struct ProgressThrottle {
    interval: Duration,
    last: Option<Instant>,
}

impl ProgressThrottle {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Whether an event at `now` goes out; if it does, the next waits a full
    /// interval from `now`.
    pub(crate) fn ready(&mut self, now: Instant) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
}

/// Whether a file may be hashed yet (issue #181).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settle {
    /// Old enough, or with nothing to say otherwise.
    Settled,
    /// Written too recently: come back after `retry_after`.
    Unsettled { retry_after: Duration },
}

/// Whether a file last modified at `mtime` has settled at `now`: whether it
/// has gone `window` without a write.
///
/// A file being copied in is written for as long as the copy runs, so its
/// modification time stays close to now; hashing it then reads a partial
/// file, and the watcher's stream of Modify events would hash it again after
/// every debounce. A file with no modification time, or one dated in the
/// future (a clock that disagrees with the file server's), gives no evidence
/// of a copy in progress and is settled. A zero window settles everything.
pub(crate) fn settle_state(
    now: DateTime<Utc>,
    mtime: Option<DateTime<Utc>>,
    window: Duration,
) -> Settle {
    let Some(mtime) = mtime else {
        return Settle::Settled;
    };
    let Ok(age) = (now - mtime).to_std() else {
        // Negative: the file is dated after `now`.
        return Settle::Settled;
    };
    match window.checked_sub(age) {
        Some(remaining) if !remaining.is_zero() => Settle::Unsettled {
            retry_after: remaining,
        },
        _ => Settle::Settled,
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
