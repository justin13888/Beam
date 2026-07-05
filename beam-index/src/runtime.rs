//! Background indexing tasks: startup scan, filesystem watcher, and the
//! periodic-rescan backstop. These run in-process alongside the HTTP server
//! that owns a [`LocalIndexService`] -- there is no separate indexer process.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};
use uuid::Uuid;

use beam_domain::repositories::LibraryRepository;

use crate::services::clock::{Clock, RealClock};
use crate::services::enrichment::MetadataEnrichmentService;
use crate::services::index::LocalIndexService;
use crate::services::watcher::{FsWatcher, NotifyFsWatcher, PathDebouncer};

/// Spawn the metadata-enrichment sweep loop. Runs forever, sweeping
/// `interval` apart unless poked sooner via
/// [`MetadataEnrichmentService::notify_handle`] (e.g. from a scan that just
/// queued new titles). Safe to call with a service backed by
/// `NoopEnrichmentProvider` -- every sweep short-circuits immediately since
/// no providers are configured.
pub fn spawn_enrichment_worker(service: Arc<MetadataEnrichmentService>, interval: Duration) {
    tokio::spawn(async move {
        service.run(interval).await;
    });
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

/// Spawn the startup scan, filesystem watcher, and periodic-maintenance
/// background tasks for in-process indexing. Call once at server startup,
/// after the [`LocalIndexService`] is constructed.
pub fn spawn_background_indexing(
    index_service: Arc<LocalIndexService>,
    config: BackgroundIndexingConfig,
) {
    // Startup scan, spawned so the caller's server can start accepting
    // requests without waiting for it.
    {
        let index = index_service.clone();
        tokio::spawn(async move {
            info!("Running startup library scan...");
            match index.scan_all_libraries().await {
                Ok(n) => info!("Startup scan complete: {n} file(s) added"),
                Err(e) => error!("Startup scan failed: {e}"),
            }
        });
    }

    // Filesystem watcher for near-real-time reconciliation (optional).
    let watcher = if config.watch_enabled {
        match NotifyFsWatcher::new() {
            Ok(w) => {
                let watcher = Arc::new(w);
                let debounce = Duration::from_millis(config.watch_debounce_ms);
                tokio::spawn(run_watch_consumer(
                    watcher.clone(),
                    index_service.clone(),
                    debounce,
                ));
                Some(watcher)
            }
            Err(e) => {
                error!("Filesystem watcher unavailable ({e}); periodic scans only");
                None
            }
        }
    } else {
        info!("Filesystem watcher disabled by configuration");
        None
    };

    // Periodic rescan backstop, also refreshing watch registrations.
    {
        let interval = Duration::from_secs(config.scan_interval_secs);
        tokio::spawn(run_periodic_maintenance(index_service, watcher, interval));
    }
}

/// Consume filesystem-watcher events, coalescing bursts within a debounce
/// window before reconciling each affected path.
async fn run_watch_consumer(
    watcher: Arc<NotifyFsWatcher>,
    index: Arc<LocalIndexService>,
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
        let window = tokio::time::sleep(debounce);
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
            if let Err(e) = index
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
    index: Arc<LocalIndexService>,
    watcher: Option<Arc<NotifyFsWatcher>>,
    interval: Duration,
) {
    let clock = RealClock;
    let mut watched: HashSet<Uuid> = HashSet::new();
    loop {
        if let Some(watcher) = &watcher {
            refresh_watches(watcher, index.library_repo().as_ref(), &mut watched).await;
        }

        clock.sleep(interval).await;

        match index.scan_all_libraries().await {
            Ok(n) => info!("Periodic rescan complete: {n} file(s) added"),
            Err(e) => error!("Periodic rescan failed: {e}"),
        }
    }
}

/// Register a recursive watch for every library not already watched.
async fn refresh_watches(
    watcher: &NotifyFsWatcher,
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
