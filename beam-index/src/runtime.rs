//! Background indexing tasks: startup scan, filesystem watcher, and the
//! periodic-rescan backstop. These run in-process alongside the HTTP server
//! that owns a [`LocalIndexService`] -- there is no separate indexer process.
//!
//! Everything here takes its collaborators as trait objects and returns the
//! [`JoinHandle`]s it spawned. Previously these were bare `tokio::spawn`s over
//! concrete types with a `RealClock` constructed inside the loop, which made
//! the whole file unobservable: a test could not tell a task from a hang, and
//! the rescan cadence could only be exercised by waiting for it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use uuid::Uuid;

use beam_domain::repositories::LibraryRepository;
use beam_domain::services::{Clock, RealClock};

use crate::services::enrichment::MetadataEnrichmentService;
use crate::services::filesystem_probe::StatfsFilesystemProbe;
use crate::services::index::{IndexError, IndexService, LocalIndexService};
use crate::services::watch_status::{WatchMode, WatchStatus, read_max_user_watches};
use crate::services::watcher::{FsEventKind, FsWatcher, NotifyFsWatcher, PathDebouncer};

/// The slice of the indexer the background tasks actually use.
///
/// A narrow trait rather than `Arc<LocalIndexService>` so the loops below can
/// be driven by a double: the point of these tasks is *when* and *how often*
/// they call the indexer, which is not observable through a concrete type.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait BackgroundIndexer: Send + Sync + std::fmt::Debug {
    /// Scan every library, returning the number of files added.
    async fn scan_all_libraries(&self) -> Result<u32, IndexError>;

    /// Scan one library, returning the number of files added. Used when a
    /// library starts being polled: the poller only reports changes made
    /// after it took its snapshot.
    async fn scan_library(&self, library_id: Uuid) -> Result<u32, IndexError>;

    /// Reconcile one path in response to a filesystem event.
    async fn reconcile_path(
        &self,
        library_id: Uuid,
        path: PathBuf,
        kind: FsEventKind,
    ) -> Result<(), IndexError>;

    /// The repository the watch refresher lists libraries from.
    fn library_repo(&self) -> Arc<dyn LibraryRepository>;
}

#[async_trait::async_trait]
impl BackgroundIndexer for LocalIndexService {
    async fn scan_all_libraries(&self) -> Result<u32, IndexError> {
        LocalIndexService::scan_all_libraries(self).await
    }

    async fn scan_library(&self, library_id: Uuid) -> Result<u32, IndexError> {
        IndexService::scan_library(self, library_id.to_string()).await
    }

    async fn reconcile_path(
        &self,
        library_id: Uuid,
        path: PathBuf,
        kind: FsEventKind,
    ) -> Result<(), IndexError> {
        LocalIndexService::reconcile_path(self, library_id, path, kind).await
    }

    fn library_repo(&self) -> Arc<dyn LibraryRepository> {
        LocalIndexService::library_repo(self).clone()
    }
}

/// Spawn the metadata-enrichment sweep loop. Runs forever, sweeping
/// `interval` apart unless poked sooner via
/// [`MetadataEnrichmentService::notify_handle`] (e.g. from a scan that just
/// queued new titles). Safe to call with a service backed by
/// `NoopEnrichmentProvider` -- every sweep short-circuits immediately since
/// no providers are configured.
pub fn spawn_enrichment_worker(
    service: Arc<MetadataEnrichmentService>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        service.run(interval).await;
    })
}

/// Configuration for beam-index's background scanning/watching tasks.
#[derive(Debug, Clone)]
pub struct BackgroundIndexingConfig {
    /// Interval between periodic full rescans of every library, in seconds.
    /// Acts as the backstop that catches changes the watcher missed.
    pub scan_interval_secs: u64,
    /// Whether to run the filesystem watcher. When false, only the startup
    /// scan and the periodic rescans run.
    pub watch_enabled: bool,
    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    pub watch_debounce_ms: u64,
    /// Interval between walks of every library the watcher polls rather than
    /// watches natively (network filesystems, and libraries past the watch
    /// limit), in seconds.
    pub watch_poll_interval_secs: u64,
}

/// The tasks [`spawn_background_indexing`] started, so a caller can await or
/// abort them instead of losing the handles.
#[derive(Debug)]
pub struct BackgroundIndexingTasks {
    /// `None` when the watcher is disabled.
    pub watch_consumer: Option<JoinHandle<()>>,
    /// Drives the watcher's polled libraries. `None` when the watcher is
    /// disabled.
    pub watch_poller: Option<JoinHandle<()>>,
    /// Registers the watches, runs the startup scan, then rescans every
    /// library once per interval.
    pub periodic_maintenance: JoinHandle<()>,
}

impl BackgroundIndexingTasks {
    /// Stop every task. Used by tests and by a graceful shutdown.
    pub fn abort(&self) {
        if let Some(handle) = &self.watch_consumer {
            handle.abort();
        }
        if let Some(handle) = &self.watch_poller {
            handle.abort();
        }
        self.periodic_maintenance.abort();
    }
}

/// Spawn the startup scan, filesystem watcher, and periodic-maintenance
/// background tasks for in-process indexing. Call once at server startup,
/// after the [`LocalIndexService`] is constructed.
///
/// `watch_status` is where the watcher reports how each library is watched;
/// the admin status endpoint reads the same instance.
pub fn spawn_background_indexing(
    index_service: Arc<LocalIndexService>,
    config: BackgroundIndexingConfig,
    watch_status: Arc<WatchStatus>,
) -> BackgroundIndexingTasks {
    let indexer: Arc<dyn BackgroundIndexer> = index_service;

    // Filesystem watcher for near-real-time reconciliation (optional).
    let watcher: Option<Arc<dyn FsWatcher>> = if config.watch_enabled {
        watch_status.set_enabled(true);
        watch_status.set_max_user_watches(read_max_user_watches());
        Some(Arc::new(NotifyFsWatcher::new(
            Arc::new(StatfsFilesystemProbe),
            watch_status,
        )))
    } else {
        info!("Filesystem watcher disabled by configuration");
        None
    };

    spawn_background_indexing_with(indexer, watcher, Arc::new(RealClock), config)
}

/// [`spawn_background_indexing`] with every collaborator injected.
pub fn spawn_background_indexing_with(
    indexer: Arc<dyn BackgroundIndexer>,
    watcher: Option<Arc<dyn FsWatcher>>,
    clock: Arc<dyn Clock>,
    config: BackgroundIndexingConfig,
) -> BackgroundIndexingTasks {
    // Sent by the maintenance task once the startup scan has finished; the
    // poller waits for it. Dropped unused when the watcher is disabled.
    let (startup_scan_done, startup_scan_finished) = oneshot::channel();

    let watch_consumer = watcher.clone().map(|watcher| {
        tokio::spawn(run_watch_consumer(
            watcher,
            indexer.clone(),
            clock.clone(),
            Duration::from_millis(config.watch_debounce_ms),
        ))
    });

    let watch_poller = watcher.clone().map(|watcher| {
        tokio::spawn(run_watch_poller(
            watcher,
            indexer.clone(),
            clock.clone(),
            Duration::from_secs(config.watch_poll_interval_secs),
            startup_scan_finished,
        ))
    });

    let periodic_maintenance = tokio::spawn(run_periodic_maintenance(
        indexer,
        watcher,
        clock,
        Duration::from_secs(config.scan_interval_secs),
        startup_scan_done,
    ));

    BackgroundIndexingTasks {
        watch_consumer,
        watch_poller,
        periodic_maintenance,
    }
}

/// Consume filesystem-watcher events, coalescing bursts within a debounce
/// window before reconciling each affected path.
async fn run_watch_consumer(
    watcher: Arc<dyn FsWatcher>,
    indexer: Arc<dyn BackgroundIndexer>,
    clock: Arc<dyn Clock>,
    debounce: Duration,
) {
    loop {
        // Block until the first event of a burst.
        let Some(first) = watcher.next_event().await else {
            info!("Filesystem watcher closed; stopping consumer");
            return;
        };
        let mut debouncer = PathDebouncer::new();
        debouncer.submit(first);

        // Collect further events for the debounce window.
        let window = clock.sleep(debounce);
        tokio::pin!(window);
        loop {
            tokio::select! {
                _ = &mut window => break,
                event = watcher.next_event() => match event {
                    Some(e) => debouncer.submit(e),
                    None => break,
                },
            }
        }

        // Reconcile the coalesced events.
        for event in debouncer.drain() {
            let path = event.path.clone();
            if let Err(e) = indexer
                .reconcile_path(event.library_id, event.path, event.kind)
                .await
            {
                warn!("Failed to reconcile {}: {e}", path.display());
            }
        }
    }
}

/// Poll the watcher's polled libraries once per `interval`, and reconcile
/// every library the poll moved off its native watch.
///
/// Nothing is polled until the startup scan has finished. A library demoted
/// during it would otherwise be scanned alongside it -- the same files hashed
/// twice, and the two scans racing to insert the same rows. Waiting loses no
/// change: whatever the poller's snapshot has not reported yet, the first
/// poll does.
///
/// The poll runs on the blocking pool: a watch-limit demotion walks the
/// library's tree to take the poller's snapshot, and the watcher's calls into
/// `notify` wait on its threads.
async fn run_watch_poller(
    watcher: Arc<dyn FsWatcher>,
    indexer: Arc<dyn BackgroundIndexer>,
    clock: Arc<dyn Clock>,
    interval: Duration,
    startup_scan_finished: oneshot::Receiver<()>,
) {
    // A dropped sender means the maintenance task is gone (aborted or
    // panicked); polling on beats leaving the polled libraries unwatched.
    let _ = startup_scan_finished.await;
    loop {
        clock.sleep(interval).await;
        let demoted = {
            let watcher = watcher.clone();
            match tokio::task::spawn_blocking(move || watcher.poll_once()).await {
                Ok(demoted) => demoted,
                Err(e) => {
                    error!("Filesystem poll failed: {e}");
                    continue;
                }
            }
        };
        for library_id in demoted {
            reconcile_newly_polled(indexer.as_ref(), library_id).await;
        }
    }
}

/// Scan a library that has just started being polled.
///
/// The poller reports only changes made after its snapshot, and the snapshot
/// is taken when the library is handed to it. Whatever changed before --
/// for a demoted library, the very directory whose watch hit the limit --
/// would otherwise wait for the next periodic rescan.
async fn reconcile_newly_polled(indexer: &dyn BackgroundIndexer, library_id: Uuid) {
    match indexer.scan_library(library_id).await {
        Ok(n) => info!(%library_id, "Scanned a newly polled library: {n} file(s) added"),
        Err(e) => warn!(%library_id, "Failed to scan a newly polled library: {e}"),
    }
}

/// Register the watches and run the startup scan, then periodically rescan
/// every library as a backstop for events the watcher missed, and bring the
/// watches in line with the libraries created or deleted since.
///
/// The watches are registered *before* the startup scan, and the scan runs
/// here rather than in a task of its own. A polled library's changes are
/// measured against a snapshot taken when it is registered, so registering
/// first means the startup scan covers everything that snapshot misses, and
/// the library needs no scan of its own. Registering alongside the scan
/// instead scanned each polled library twice at once: its files hashed twice
/// over the network, and the two scans racing to insert the same rows.
///
/// `startup_scan_done` is sent once the startup scan has finished, to release
/// the poller.
async fn run_periodic_maintenance(
    indexer: Arc<dyn BackgroundIndexer>,
    watcher: Option<Arc<dyn FsWatcher>>,
    clock: Arc<dyn Clock>,
    interval: Duration,
    startup_scan_done: oneshot::Sender<()>,
) {
    if let Some(watcher) = &watcher {
        refresh_watches(watcher, indexer.as_ref(), NewlyPolled::CoveredByStartupScan).await;
    }

    info!("Running startup library scan...");
    match indexer.scan_all_libraries().await {
        Ok(n) => info!("Startup scan complete: {n} file(s) added"),
        Err(e) => error!("Startup scan failed: {e}"),
    }
    // Nobody is listening only when the watcher is disabled.
    let _ = startup_scan_done.send(());

    loop {
        clock.sleep(interval).await;

        match indexer.scan_all_libraries().await {
            Ok(n) => info!("Periodic rescan complete: {n} file(s) added"),
            Err(e) => error!("Periodic rescan failed: {e}"),
        }

        if let Some(watcher) = &watcher {
            refresh_watches(watcher, indexer.as_ref(), NewlyPolled::Scan).await;
        }
    }
}

/// What [`refresh_watches`] does with a library it registers as polled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewlyPolled {
    /// Scan it once (see [`reconcile_newly_polled`]).
    Scan,
    /// Leave it: the startup scan, which has not started yet, covers it.
    CoveredByStartupScan,
}

/// Bring the watches in line with the libraries that exist: stop watching
/// every library that has been deleted, then register a recursive watch for
/// every library not already watched.
///
/// This is the backstop for library creation and deletion -- it runs once per
/// maintenance cycle. Deleted libraries are unwatched first, so a library
/// re-created at a deleted one's root is registered after the old watch is
/// gone. A library that comes back polled is scanned once (see
/// [`reconcile_newly_polled`]) unless `newly_polled` says the startup scan
/// covers it.
///
/// What is already watched is asked of the watcher, not remembered here: the
/// watcher drops a registration on its own when it cannot move a library to
/// the poller, and a copy kept here would never register that library again.
///
/// The watcher calls run on the blocking pool: registering a library walks
/// its tree, natively or to take the poller's snapshot.
async fn refresh_watches(
    watcher: &Arc<dyn FsWatcher>,
    indexer: &dyn BackgroundIndexer,
    newly_polled: NewlyPolled,
) {
    let libraries = match indexer.library_repo().find_all().await {
        Ok(libraries) => libraries,
        Err(e) => {
            warn!("Watch refresh failed to list libraries: {e}");
            return;
        }
    };

    let live: HashSet<Uuid> = libraries.iter().map(|library| library.id).collect();
    let watched: HashSet<Uuid> = watcher.registered_libraries().into_iter().collect();
    let deleted: Vec<Uuid> = watched.difference(&live).copied().collect();
    for library_id in deleted {
        // Not retried whatever the outcome: the watcher drops its
        // registration before it calls into the backend.
        let watcher = watcher.clone();
        match tokio::task::spawn_blocking(move || watcher.unwatch_library(library_id)).await {
            Ok(Ok(())) => info!(%library_id, "Stopped watching a deleted library"),
            Ok(Err(e)) => warn!(%library_id, "Failed to unwatch a deleted library: {e}"),
            Err(e) => error!(%library_id, "Unwatching a deleted library failed: {e}"),
        }
    }

    for library in libraries {
        if watched.contains(&library.id) {
            continue;
        }
        let registration = {
            let watcher = watcher.clone();
            let library_id = library.id;
            let root = library.root_path.clone();
            tokio::task::spawn_blocking(move || watcher.watch_library(library_id, &root)).await
        };
        match registration {
            Ok(Ok(mode)) => {
                info!(
                    "Watching library '{}' at {} ({mode:?})",
                    library.name,
                    library.root_path.display()
                );
                if matches!(mode, WatchMode::Polling(_)) && newly_polled == NewlyPolled::Scan {
                    reconcile_newly_polled(indexer, library.id).await;
                }
            }
            // Not registered, so the next maintenance cycle retries.
            Ok(Err(e)) => warn!("Failed to watch library '{}': {e}", library.name),
            Err(e) => error!("Watching library '{}' failed: {e}", library.name),
        }
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod runtime_tests;
