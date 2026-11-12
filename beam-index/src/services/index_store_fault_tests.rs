//! NFR-205: what a scan does when one of the stores beside the file store
//! fails under it -- the applied-NFO record, the sidecar subtitles, the
//! chapters and the enrichment queue. #378 covers the file store's own
//! `mark_missing` and `purge_missing`.
//!
//! Each failure is produced through the injected trait: the harness wires
//! every one of these stores through a wrapper that answers through the
//! in-memory store and fails its writes while the test says so. The test
//! then reads what the failure left behind through the in-memory store, and
//! that the next scan after the store recovers repairs it.

use super::*;
use beam_domain::models::applied_nfo::RecordAppliedNfo;
use beam_domain::models::enrichment::{
    EnrichmentListFilter, EnrichmentListQuery, EnrichmentStatus, EnrichmentStatusCounts, FieldLocks,
};
use beam_domain::models::sidecar::UpsertSidecarSubtitle;
use beam_domain::repositories::{ChapterRepository, SidecarSubtitleRepository};

fn database_down() -> DbErr {
    DbErr::Custom("the database is down".to_string())
}

/// Which stores fail their writes, as the test sets them.
#[derive(Debug, Default)]
pub(super) struct StoreFaults {
    pub(super) applied_nfos: AtomicBool,
    pub(super) sidecars: AtomicBool,
    pub(super) chapters: AtomicBool,
    pub(super) enrichment: AtomicBool,
}

impl StoreFaults {
    fn failing(flag: &AtomicBool) -> Result<(), DbErr> {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            Err(database_down())
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub(super) struct FaultyAppliedNfos {
    pub(super) inner: Arc<InMemoryAppliedNfoRepository>,
    pub(super) faults: Arc<StoreFaults>,
}

#[async_trait::async_trait]
impl AppliedNfoRepository for FaultyAppliedNfos {
    async fn record_by_path(&self, record: RecordAppliedNfo) -> Result<AppliedNfo, DbErr> {
        StoreFaults::failing(&self.faults.applied_nfos)?;
        self.inner.record_by_path(record).await
    }

    async fn find_by_path(&self, path: &Path) -> Result<Option<AppliedNfo>, DbErr> {
        self.inner.find_by_path(path).await
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<AppliedNfo>, DbErr> {
        self.inner.find_all_by_library(library_id).await
    }

    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        StoreFaults::failing(&self.faults.applied_nfos)?;
        self.inner.delete_by_ids(ids).await
    }

    async fn delete_beneath(&self, library_id: Uuid, dir: &Path) -> Result<u64, DbErr> {
        StoreFaults::failing(&self.faults.applied_nfos)?;
        self.inner.delete_beneath(library_id, dir).await
    }
}

#[derive(Debug)]
pub(super) struct FaultySidecars {
    pub(super) inner: Arc<InMemorySidecarSubtitleRepository>,
    pub(super) faults: Arc<StoreFaults>,
}

#[async_trait::async_trait]
impl SidecarSubtitleRepository for FaultySidecars {
    async fn upsert_by_path(
        &self,
        upsert: UpsertSidecarSubtitle,
    ) -> Result<SidecarSubtitle, DbErr> {
        StoreFaults::failing(&self.faults.sidecars)?;
        self.inner.upsert_by_path(upsert).await
    }

    async fn find_by_path(&self, path: &Path) -> Result<Option<SidecarSubtitle>, DbErr> {
        self.inner.find_by_path(path).await
    }

    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr> {
        self.inner.find_by_file_id(file_id).await
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr> {
        self.inner.find_all_by_library(library_id).await
    }

    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        StoreFaults::failing(&self.faults.sidecars)?;
        self.inner.delete_by_ids(ids).await
    }
}

#[derive(Debug)]
pub(super) struct FaultyChapters {
    pub(super) inner: Arc<InMemoryChapterRepository>,
    pub(super) faults: Arc<StoreFaults>,
}

#[async_trait::async_trait]
impl ChapterRepository for FaultyChapters {
    async fn replace_for_file(&self, file_id: Uuid, chapters: Vec<Chapter>) -> Result<(), DbErr> {
        StoreFaults::failing(&self.faults.chapters)?;
        self.inner.replace_for_file(file_id, chapters).await
    }

    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<Chapter>, DbErr> {
        self.inner.find_by_file_id(file_id).await
    }
}

#[derive(Debug)]
pub(super) struct FaultyEnrichmentQueue {
    pub(super) inner: Arc<InMemoryEnrichmentStateRepository>,
    pub(super) faults: Arc<StoreFaults>,
}

#[async_trait::async_trait]
impl EnrichmentStateRepository for FaultyEnrichmentQueue {
    async fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr> {
        StoreFaults::failing(&self.faults.enrichment)?;
        self.inner.ensure_pending(target).await
    }

    async fn fetch_due(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<EnrichmentState>, DbErr> {
        self.inner.fetch_due(now, limit).await
    }

    async fn find_by_target(
        &self,
        target: EnrichmentTargetId,
    ) -> Result<Option<EnrichmentState>, DbErr> {
        self.inner.find_by_target(target).await
    }

    async fn mark_enriched(
        &self,
        id: Uuid,
        matched_ref: &str,
        confidence: f32,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        self.inner
            .mark_enriched(id, matched_ref, confidence, now)
            .await
    }

    async fn mark_unmatched(
        &self,
        id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        self.inner.mark_unmatched(id, reason, now).await
    }

    async fn mark_retrying(
        &self,
        id: Uuid,
        error: &str,
        attempts: u32,
        next_attempt_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        self.inner
            .mark_retrying(id, error, attempts, next_attempt_at, now)
            .await
    }

    async fn mark_failed(&self, id: Uuid, error: &str, now: DateTime<Utc>) -> Result<(), DbErr> {
        self.inner.mark_failed(id, error, now).await
    }

    async fn request_refresh(
        &self,
        target: EnrichmentTargetId,
        rematch: bool,
    ) -> Result<bool, DbErr> {
        StoreFaults::failing(&self.faults.enrichment)?;
        self.inner.request_refresh(target, rematch).await
    }

    async fn request_refresh_library(&self, library_id: Uuid, rematch: bool) -> Result<u64, DbErr> {
        self.inner
            .request_refresh_library(library_id, rematch)
            .await
    }

    async fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr> {
        self.inner.request_refresh_all(rematch).await
    }

    async fn set_locked_fields(
        &self,
        target: EnrichmentTargetId,
        locks: &FieldLocks,
    ) -> Result<EnrichmentState, DbErr> {
        self.inner.set_locked_fields(target, locks).await
    }

    async fn list(&self, query: &EnrichmentListQuery) -> Result<Vec<EnrichmentState>, DbErr> {
        self.inner.list(query).await
    }

    async fn count(&self, filter: &EnrichmentListFilter) -> Result<u64, DbErr> {
        self.inner.count(filter).await
    }

    async fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr> {
        self.inner.count_by_status().await
    }
}

fn fail(flag: &AtomicBool) {
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn heal(flag: &AtomicBool) {
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// A new video whose NFO's application cannot be recorded fails as a file
/// the scan could not record: it is not indexed, the scan goes on, and the
/// next scan after the store recovers indexes it, pinned by the NFO, and
/// records the NFO it applied.
#[tokio::test]
async fn a_video_whose_nfo_cannot_be_recorded_is_indexed_by_the_next_scan() {
    let h = Harness::new().await;
    h.video("The Matrix (1999)/The Matrix (1999).mkv");
    h.video("Heat (1995).mkv");
    h.write("The Matrix (1999)/The Matrix (1999).nfo", MATRIX_NFO);
    fail(&h.faults.applied_nfos);

    h.scan().await;

    let matrix = h.root.join("The Matrix (1999)/The Matrix (1999).mkv");
    assert!(
        h.file_repo
            .find_by_path(&matrix.to_string_lossy())
            .await
            .unwrap()
            .is_none(),
        "not indexed while its NFO cannot be recorded"
    );
    assert!(
        h.file("Heat (1995).mkv").content.is_some(),
        "the scan went on"
    );
    assert!(
        h.applied("The Matrix (1999)/The Matrix (1999).nfo")
            .await
            .is_none()
    );

    heal(&h.faults.applied_nfos);
    h.scan().await;

    let applied = h
        .applied("The Matrix (1999)/The Matrix (1999).nfo")
        .await
        .expect("the NFO's application is recorded");
    assert_eq!(applied.library_id, h.library.id);
    assert_eq!(
        h.movie_of("The Matrix (1999)/The Matrix (1999).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:603")
    );
}

/// A subtitle store that fails costs only the subtitles: the video is
/// indexed, and the next scan after the store recovers records them.
#[tokio::test]
async fn a_subtitle_that_cannot_be_recorded_is_recorded_by_the_next_scan() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).en.srt", "1");
    fail(&h.faults.sidecars);

    h.scan().await;

    let video = h.file("Heat (1995)/Heat (1995).mkv");
    assert!(video.content.is_some(), "the video is indexed all the same");
    assert!(
        h.subtitles_of("Heat (1995)/Heat (1995).mkv")
            .await
            .is_empty()
    );

    heal(&h.faults.sidecars);
    h.scan().await;

    let subtitles: Vec<PathBuf> = h
        .subtitles_of("Heat (1995)/Heat (1995).mkv")
        .await
        .into_iter()
        .map(|subtitle| subtitle.path)
        .collect();
    assert_eq!(subtitles, [h.root.join("Heat (1995)/Heat (1995).en.srt")]);
}

/// A probe whose chapters cannot be stored still indexes its file: the scan
/// goes on, and the file is classified with the runtime the probe read.
///
/// Not pinned here: whether a later scan stores the chapters once the store
/// recovers. Today it does not -- the file is recorded as probed, so an
/// unchanged file is never probed again -- and that is filed as its own
/// defect rather than characterized as intended.
#[tokio::test]
async fn chapters_that_cannot_be_stored_do_not_cost_the_file_its_index() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.chapter(
        "Heat (1995)/Heat (1995).mkv",
        &[(0, 60_000, Some("Opening"))],
    );
    fail(&h.faults.chapters);

    h.scan().await;

    assert!(
        h.chapters_of("Heat (1995)/Heat (1995).mkv")
            .await
            .is_empty()
    );
    let video = h.file("Heat (1995)/Heat (1995).mkv");
    assert_eq!(video.status, FileStatus::Known);
    assert_eq!(video.duration, Some(Duration::from_secs(60 * 60)));
    assert_eq!(h.movie_of("Heat (1995)/Heat (1995).mkv").title, "Heat");
}

/// A new video whose title cannot be queued for enrichment fails as a file
/// the scan could not record, and the next scan after the queue recovers
/// indexes it and queues its title.
#[tokio::test]
async fn a_title_that_cannot_be_queued_for_enrichment_is_queued_by_the_next_scan() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    fail(&h.faults.enrichment);

    h.scan().await;

    let heat = h.root.join("Heat (1995)/Heat (1995).mkv");
    assert!(
        h.file_repo
            .find_by_path(&heat.to_string_lossy())
            .await
            .unwrap()
            .is_none(),
        "not indexed while its title cannot be queued"
    );

    assert!(
        h.enrichment_repo
            .fetch_due(chrono::Utc::now() + chrono::Duration::days(1), 1000)
            .await
            .unwrap()
            .is_empty()
    );

    heal(&h.faults.enrichment);
    h.scan().await;

    let movie = h.movie_of("Heat (1995)/Heat (1995).mkv");
    let state = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert_eq!(state.status, EnrichmentStatus::Pending);
}
