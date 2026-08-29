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

use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use uuid::Uuid;

use beam_domain::repositories::LibraryRepository;
use beam_domain::services::{Clock, RealClock};

use crate::services::enrichment::MetadataEnrichmentService;
use crate::services::index::{IndexError, LocalIndexService};
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
    /// Whether to run the inotify-based filesystem watcher. When false, only
    /// the startup scan and the periodic rescans run.
    pub watch_enabled: bool,
    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    pub watch_debounce_ms: u64,
}

/// The tasks [`spawn_background_indexing`] started, so a caller can await or
/// abort them instead of losing the handles.
#[derive(Debug)]
pub struct BackgroundIndexingTasks {
    pub startup_scan: JoinHandle<()>,
    /// `None` when the watcher is disabled or could not be created.
    pub watch_consumer: Option<JoinHandle<()>>,
    pub periodic_maintenance: JoinHandle<()>,
}

impl BackgroundIndexingTasks {
    /// Stop every task. Used by tests and by a graceful shutdown.
    pub fn abort(&self) {
        self.startup_scan.abort();
        if let Some(handle) = &self.watch_consumer {
            handle.abort();
        }
        self.periodic_maintenance.abort();
    }
}

/// Spawn the startup scan, filesystem watcher, and periodic-maintenance
/// background tasks for in-process indexing. Call once at server startup,
/// after the [`LocalIndexService`] is constructed.
pub fn spawn_background_indexing(
    index_service: Arc<LocalIndexService>,
    config: BackgroundIndexingConfig,
) -> BackgroundIndexingTasks {
    let indexer: Arc<dyn BackgroundIndexer> = index_service;

    // Filesystem watcher for near-real-time reconciliation (optional).
    let watcher: Option<Arc<dyn FsWatcher>> = if config.watch_enabled {
        match NotifyFsWatcher::new() {
            Ok(w) => Some(Arc::new(w)),
            Err(e) => {
                error!("Filesystem watcher unavailable ({e}); periodic scans only");
                None
            }
        }
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
    // Startup scan, spawned so the caller's server can start accepting
    // requests without waiting for it.
    let startup_scan = {
        let indexer = indexer.clone();
        tokio::spawn(async move {
            info!("Running startup library scan...");
            match indexer.scan_all_libraries().await {
                Ok(n) => info!("Startup scan complete: {n} file(s) added"),
                Err(e) => error!("Startup scan failed: {e}"),
            }
        })
    };

    let watch_consumer = watcher.clone().map(|watcher| {
        tokio::spawn(run_watch_consumer(
            watcher,
            indexer.clone(),
            clock.clone(),
            Duration::from_millis(config.watch_debounce_ms),
        ))
    });

    let periodic_maintenance = tokio::spawn(run_periodic_maintenance(
        indexer,
        watcher,
        clock,
        Duration::from_secs(config.scan_interval_secs),
    ));

    BackgroundIndexingTasks {
        startup_scan,
        watch_consumer,
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

/// Periodically rescan every library as a backstop for events the watcher
/// missed, and register watches for libraries created since startup.
async fn run_periodic_maintenance(
    indexer: Arc<dyn BackgroundIndexer>,
    watcher: Option<Arc<dyn FsWatcher>>,
    clock: Arc<dyn Clock>,
    interval: Duration,
) {
    let mut watched: HashSet<Uuid> = HashSet::new();
    loop {
        if let Some(watcher) = &watcher {
            refresh_watches(
                watcher.as_ref(),
                indexer.library_repo().as_ref(),
                &mut watched,
            )
            .await;
        }

        clock.sleep(interval).await;

        match indexer.scan_all_libraries().await {
            Ok(n) => info!("Periodic rescan complete: {n} file(s) added"),
            Err(e) => error!("Periodic rescan failed: {e}"),
        }
    }
}

/// Register a recursive watch for every library not already watched.
async fn refresh_watches(
    watcher: &dyn FsWatcher,
    library_repo: &dyn LibraryRepository,
    watched: &mut HashSet<Uuid>,
) {
    let libraries = match library_repo.find_all().await {
        Ok(libraries) => libraries,
        Err(e) => {
            warn!("Watch refresh failed to list libraries: {e}");
            return;
        }
    };
    for library in libraries {
        if watched.insert(library.id) {
            match watcher.watch_library(library.id, &library.root_path) {
                Ok(()) => info!(
                    "Watching library '{}' at {}",
                    library.name,
                    library.root_path.display()
                ),
                Err(e) => {
                    warn!("Failed to watch library '{}': {e}", library.name);
                    // Allow a retry on the next maintenance cycle.
                    watched.remove(&library.id);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod runtime_tests;
