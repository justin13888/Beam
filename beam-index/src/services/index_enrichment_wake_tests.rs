//! Waking the enrichment worker when the indexer queues titles (issue #449).
//!
//! A scan or watcher reconcile that queues a title for enrichment wakes the
//! worker once as it ends, however many titles it queued, and one that
//! queues none does not wake it. These run the real scan and reconcile over a
//! `TempDir` library, built by the shared [`IndexServiceBuilder`] over wired
//! in-memory stores, with a wake that counts.

use super::test_harness::*;
use super::*;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use std::sync::atomic::AtomicUsize;
use tempfile::TempDir;

/// How many times the indexer woke the worker.
#[derive(Debug, Default)]
struct CountingWake(AtomicUsize);

impl CountingWake {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl EnrichmentWake for CountingWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library_id: Uuid,
    enrichment_repo: Arc<InMemoryEnrichmentStateRepository>,
    wake: Arc<CountingWake>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let stores = IndexStores::wired();
        let enrichment_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
        let library = make_library_in_tempdir(&stores.library_repo, &dir).await;

        // Every file gets its own hash, so none reads as another's copy.
        let next_hash = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut hasher = MockHashService::new();
        hasher
            .expect_hash_async()
            .returning(move |_| Ok(next_hash.fetch_add(1, Ordering::Relaxed)));
        let mut prober = MockMediaInfoService::new();
        prober.expect_get_video_metadata().returning(|file| {
            let mut metadata = make_video_metadata();
            metadata.file_path = file.path().to_path_buf();
            Ok(metadata)
        });

        let wake = Arc::new(CountingWake::default());
        let service = IndexServiceBuilder::new()
            .stores(&stores)
            .hash_service(Arc::new(hasher))
            .media_info(Arc::new(prober))
            .build()
            .with_enrichment_repo(enrichment_repo.clone())
            .with_enrichment_wake(wake.clone());

        Self {
            root: dir.path().to_path_buf(),
            _dir: dir,
            library_id: library.id,
            enrichment_repo,
            wake,
            service,
        }
    }

    fn write(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
        path
    }

    /// How many titles are queued for enrichment.
    async fn queued(&self) -> usize {
        self.enrichment_repo
            .fetch_due(chrono::Utc::now(), 100)
            .await
            .unwrap()
            .len()
    }
}

#[tokio::test]
async fn a_scan_that_queues_several_titles_wakes_the_worker_once() {
    let h = Harness::new().await;
    h.write("Alien (1979)/Alien (1979).mkv");
    h.write("Heat (1995)/Heat (1995).mkv");
    h.write("Severance/Season 01/Severance - S01E01.mkv");

    h.service.scan_library(h.library_id).await.unwrap();

    assert_eq!(h.queued().await, 3, "the scan queued every new title");
    assert_eq!(h.wake.count(), 1, "one wake for the whole scan");
}

#[tokio::test]
async fn a_scan_that_queues_nothing_does_not_wake_the_worker() {
    let h = Harness::new().await;
    h.write("Alien (1979)/Alien (1979).mkv");
    h.service.scan_library(h.library_id).await.unwrap();
    assert_eq!(h.wake.count(), 1);

    // Nothing changed on disk: no file is classified, no title queued.
    h.service.scan_library(h.library_id).await.unwrap();

    assert_eq!(h.wake.count(), 1, "an unchanged library wakes nobody");
}

#[tokio::test]
async fn an_empty_library_scan_does_not_wake_the_worker() {
    let h = Harness::new().await;

    h.service.scan_library(h.library_id).await.unwrap();

    assert_eq!(h.queued().await, 0);
    assert_eq!(h.wake.count(), 0);
}

#[tokio::test]
async fn a_reconcile_that_indexes_a_new_title_wakes_the_worker_once() {
    let h = Harness::new().await;
    h.service.scan_library(h.library_id).await.unwrap();
    let path = h.write("Heat (1995)/Heat (1995).mkv");

    let outcome = h
        .service
        .reconcile_path(h.library_id, path, FsEventKind::Created)
        .await
        .unwrap();

    assert!(matches!(outcome, ReconcileOutcome::Done));
    assert_eq!(h.queued().await, 1, "the reconcile queued the new title");
    assert_eq!(h.wake.count(), 1);
}

#[tokio::test]
async fn a_reconcile_of_a_new_folder_wakes_the_worker_once_for_all_its_titles() {
    let h = Harness::new().await;
    h.service.scan_library(h.library_id).await.unwrap();
    h.write("Movies/Alien (1979)/Alien (1979).mkv");
    h.write("Movies/Heat (1995)/Heat (1995).mkv");

    h.service
        .reconcile_path(h.library_id, h.root.join("Movies"), FsEventKind::Created)
        .await
        .unwrap();

    assert_eq!(h.queued().await, 2);
    assert_eq!(h.wake.count(), 1, "one wake for the whole folder");
}

#[tokio::test]
async fn a_reconcile_that_queues_nothing_does_not_wake_the_worker() {
    let h = Harness::new().await;
    let path = h.write("Alien (1979)/Alien (1979).mkv");
    h.service.scan_library(h.library_id).await.unwrap();
    assert_eq!(h.wake.count(), 1);

    // An unchanged file, then the same file gone: neither queues a title.
    h.service
        .reconcile_path(h.library_id, path.clone(), FsEventKind::Modified)
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    h.service
        .reconcile_path(h.library_id, path, FsEventKind::Removed)
        .await
        .unwrap();

    assert_eq!(h.wake.count(), 1, "no event queued a title");
}
