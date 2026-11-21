//! What a scan and the watcher make of the metadata a library keeps beside
//! its media (issue #184): Kodi NFOs, container tags and sidecar subtitles --
//! and that reading them never writes a byte into the library.
//!
//! These run the real scan over a `TempDir` library, with in-memory doubles
//! below the repository traits. This file holds the shared harness; the
//! tests are in its `nfo`, `sidecar` and `container` modules.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;
use crate::library_file::test_support::snapshot;
use crate::probe::metadata::{ChapterMetadata, MetadataError};
use crate::services::hash::{HashService, LocalHashService, MockHashService};
use crate::services::media_info::{LocalMediaInfoService, MockMediaInfoService};
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::applied_nfo::AppliedNfo;
use beam_domain::models::enrichment::{EnrichmentState, EnrichmentTargetId};
use beam_domain::models::file::{CreateMediaFile, FileClassification, UpdateMediaFile};
use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat};
use beam_domain::models::{CreateLibrary, Movie, Show};
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::applied_nfo::in_memory::InMemoryAppliedNfoRepository;
use beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{
    AdminLogRepository, AppliedNfoRepository, EnrichmentStateRepository, FileRepository,
};
use tempfile::TempDir;

/// Container tags the prober reports, by path.
type Tags = Arc<Mutex<HashMap<PathBuf, HashMap<String, String>>>>;

/// Chapters the prober reports, by path.
type Chapters = Arc<Mutex<HashMap<PathBuf, Vec<ChapterMetadata>>>>;

/// The in-memory file repository, counting the reads that list files: how
/// many read a whole library, and how many only the files beneath a folder.
#[derive(Debug)]
struct CountingFileRepository {
    inner: Arc<InMemoryFileRepository>,
    whole_library_reads: AtomicUsize,
    folder_reads: AtomicUsize,
}

#[async_trait::async_trait]
impl FileRepository for CountingFileRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
        self.inner.find_by_id(id).await
    }
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr> {
        self.inner.find_by_path(path).await
    }
    async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr> {
        self.inner.find_by_hash(hash).await
    }
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.whole_library_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.find_all_by_library(library_id).await
    }
    async fn find_page_by_library(
        &self,
        library_id: Uuid,
        after: Option<beam_domain::models::file::LibraryFilePosition>,
        limit: std::num::NonZeroU32,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.whole_library_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .find_page_by_library(library_id, after, limit)
            .await
    }
    async fn find_all_by_library_including_missing(
        &self,
        library_id: Uuid,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.whole_library_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .find_all_by_library_including_missing(library_id)
            .await
    }
    async fn find_all_under(&self, library_id: Uuid, dir: &Path) -> Result<Vec<MediaFile>, DbErr> {
        self.folder_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.find_all_under(library_id, dir).await
    }
    async fn find_by_library_and_hash_including_missing(
        &self,
        library_id: Uuid,
        hash: u64,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.inner
            .find_by_library_and_hash_including_missing(library_id, hash)
            .await
    }
    async fn find_beneath_including_missing(
        &self,
        library_id: Uuid,
        dir: &Path,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.inner
            .find_beneath_including_missing(library_id, dir)
            .await
    }
    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.inner.find_by_movie_entry_id(movie_entry_id).await
    }
    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.inner.find_by_episode_id(episode_id).await
    }
    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr> {
        self.inner.create(create).await
    }
    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
        self.inner.update(update).await
    }
    async fn set_classification(
        &self,
        id: Uuid,
        classification: FileClassification,
    ) -> Result<MediaFile, DbErr> {
        self.inner.set_classification(id, classification).await
    }
    async fn mark_missing(&self, ids: Vec<Uuid>, at: DateTime<Utc>) -> Result<u64, DbErr> {
        self.inner.mark_missing(ids, at).await
    }
    async fn relink(
        &self,
        relinks: Vec<FileRelink>,
        displaced: Vec<Uuid>,
        at: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        self.inner.relink(relinks, displaced, at).await
    }
    async fn restore(&self, id: Uuid) -> Result<(), DbErr> {
        self.inner.restore(id).await
    }
    async fn purge_missing(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        self.inner.purge_missing(ids).await
    }
    async fn count_all(&self) -> Result<u64, DbErr> {
        self.inner.count_all().await
    }
}

/// What probes and hashes the harness's files.
enum Probe {
    /// A double reporting the tags [`Harness::tag`] sets.
    Double,
    /// The real FFmpeg prober and hasher, over real containers.
    Real,
    /// The double prober with the real hasher, so a moved file's content
    /// matches its row and it is relinked (issue #180).
    ContentHashed,
}

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    enrichment_repo: Arc<InMemoryEnrichmentStateRepository>,
    sidecar_repo: Arc<InMemorySidecarSubtitleRepository>,
    chapter_repo: Arc<InMemoryChapterRepository>,
    applied_nfo_repo: Arc<InMemoryAppliedNfoRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    file_reads: Arc<CountingFileRepository>,
    tags: Tags,
    chapters: Chapters,
    /// While set, every probe fails, as one of a file still being muxed.
    probe_fails: Arc<AtomicBool>,
    /// Which of the stores beside the file store fail their writes.
    faults: Arc<store_fault_tests::StoreFaults>,
    service: Arc<LocalIndexService>,
}

#[path = "index_store_fault_tests.rs"]
mod store_fault_tests;

/// A clock stopped at the moment it is made: before any file a test then
/// writes. No NFO written after it ever settles (`STAMP_SETTLE`), however
/// long the test takes under load, so what a scan records does not depend
/// on how quickly it followed the write.
fn pinned_clock() -> Arc<dyn Clock> {
    Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now(),
    ))
}

impl Harness {
    async fn new() -> Self {
        Self::build(Probe::Double, pinned_clock()).await
    }

    async fn with_real_prober() -> Self {
        let _ = crate::probe::init();
        Self::build(Probe::Real, pinned_clock()).await
    }

    async fn build(probe: Probe, clock: Arc<dyn Clock>) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::with_files(file_repo.clone()));
        let show_repo = Arc::new(InMemoryShowRepository::with_files(file_repo.clone()));
        let enrichment_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
        let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
        let chapter_repo = Arc::new(InMemoryChapterRepository::default());
        let applied_nfo_repo = Arc::new(InMemoryAppliedNfoRepository::default());
        let faults = Arc::new(store_fault_tests::StoreFaults::default());
        let file_reads = Arc::new(CountingFileRepository {
            inner: file_repo.clone(),
            whole_library_reads: AtomicUsize::new(0),
            folder_reads: AtomicUsize::new(0),
        });
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let library = library_repo
            .create(CreateLibrary {
                name: "Beside".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();

        let next_hash = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut hasher = MockHashService::new();
        hasher
            .expect_hash_async()
            .returning(move |_| Ok(next_hash.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let tags: Tags = Arc::default();
        let prober_tags = tags.clone();
        let chapters: Chapters = Arc::default();
        let prober_chapters = chapters.clone();
        let probe_fails = Arc::new(AtomicBool::new(false));
        let prober_fails = probe_fails.clone();
        let mut prober = MockMediaInfoService::new();
        prober.expect_get_video_metadata().returning(move |file| {
            let path = file.path();
            if prober_fails.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(MetadataError::UnknownError("not probeable yet".to_string()));
            }
            Ok(VideoFileMetadata {
                file_path: path.to_path_buf(),
                metadata: prober_tags
                    .lock()
                    .unwrap()
                    .get(path)
                    .cloned()
                    .unwrap_or_default(),
                best_video_stream: None,
                best_audio_stream: None,
                best_subtitle_stream: None,
                duration: 3_600_000_000,
                streams: vec![],
                format_name: "matroska".to_string(),
                format_long_name: "Matroska".to_string(),
                file_size: 1024,
                bit_rate: 1000,
                probe_score: 100,
                chapters: prober_chapters
                    .lock()
                    .unwrap()
                    .get(path)
                    .cloned()
                    .unwrap_or_default(),
            })
        });

        let (hasher, prober): (Arc<dyn HashService>, Arc<dyn MediaInfoService>) = match probe {
            Probe::Double => (Arc::new(hasher), Arc::new(prober)),
            Probe::ContentHashed => (Arc::new(LocalHashService::default()), Arc::new(prober)),
            Probe::Real => (
                Arc::new(LocalHashService::default()),
                Arc::new(LocalMediaInfoService::default()),
            ),
        };
        let service = LocalIndexService::new(
            library_repo.clone(),
            file_reads.clone(),
            movie_repo.clone(),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            hasher,
            prober,
            Arc::new(InMemoryNotificationService::new()),
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>,
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        )
        .with_enrichment_repo(Arc::new(store_fault_tests::FaultyEnrichmentQueue {
            inner: enrichment_repo.clone(),
            faults: faults.clone(),
        }))
        .with_sidecar_repo(Arc::new(store_fault_tests::FaultySidecars {
            inner: sidecar_repo.clone(),
            faults: faults.clone(),
        }))
        .with_chapter_repo(Arc::new(store_fault_tests::FaultyChapters {
            inner: chapter_repo.clone(),
            faults: faults.clone(),
        }))
        .with_applied_nfo_repo(Arc::new(store_fault_tests::FaultyAppliedNfos {
            inner: applied_nfo_repo.clone(),
            faults: faults.clone(),
        }))
        .with_clock(clock);

        Self {
            _dir: dir,
            root,
            library,
            library_repo,
            file_repo,
            movie_repo,
            show_repo,
            enrichment_repo,
            sidecar_repo,
            chapter_repo,
            applied_nfo_repo,
            admin_log_repo,
            file_reads,
            tags,
            chapters,
            probe_fails,
            faults,
            service: Arc::new(service),
        }
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents.as_bytes()).unwrap();
        path
    }

    fn video(&self, rel: &str) -> PathBuf {
        self.write(rel, rel)
    }

    fn tag(&self, rel: &str, tags: &[(&str, &str)]) {
        self.tags.lock().unwrap().insert(
            self.root.join(rel),
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
    }

    async fn scan(&self) {
        self.service.scan_library(self.library.id).await.unwrap();
    }

    async fn event(&self, rel: &str, kind: FsEventKind) {
        assert_eq!(
            self.event_outcome(rel, kind).await,
            ReconcileOutcome::Done,
            "the event for {rel} was reconciled, not deferred"
        );
    }

    async fn event_outcome(&self, rel: &str, kind: FsEventKind) -> ReconcileOutcome {
        self.service
            .reconcile_path(self.library.id, self.root.join(rel), kind)
            .await
            .unwrap()
    }

    fn file(&self, rel: &str) -> MediaFile {
        let path = self.root.join(rel);
        self.file_repo
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .cloned()
            .unwrap_or_else(|| panic!("{rel} has a row"))
    }

    fn movie_of(&self, rel: &str) -> Movie {
        let Some(MediaFileContent::Movie { movie_entry_id, .. }) = self.file(rel).content else {
            panic!("{rel} is not a movie file");
        };
        let movie_id = self.movie_repo.entries.lock().unwrap()[&movie_entry_id].movie_id;
        self.movie_repo.movies.lock().unwrap()[&movie_id].clone()
    }

    fn show_of(&self, rel: &str) -> (Show, u32, u32) {
        let Some(MediaFileContent::Episode { episode_id, .. }) = self.file(rel).content else {
            panic!("{rel} is not an episode file");
        };
        let episode = self.show_repo.episodes.lock().unwrap()[&episode_id].clone();
        let season = self.show_repo.seasons.lock().unwrap()[&episode.season_id].clone();
        let show = self.show_repo.shows.lock().unwrap()[&season.show_id].clone();
        (show, season.season_number, episode.episode_number)
    }

    async fn enrichment_of(&self, target: EnrichmentTargetId) -> EnrichmentState {
        self.enrichment_repo
            .fetch_due(chrono::Utc::now() + chrono::Duration::days(1), 1000)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.target == target)
            .expect("the title is queued for enrichment")
    }

    /// Have the prober report `chapters` for `rel`.
    fn chapter(&self, rel: &str, chapters: &[(u64, u64, Option<&str>)]) {
        self.chapters.lock().unwrap().insert(
            self.root.join(rel),
            chapters
                .iter()
                .map(|(start_ms, end_ms, title)| ChapterMetadata {
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                    title: title.map(str::to_owned),
                })
                .collect(),
        );
    }

    async fn chapters_of(&self, rel: &str) -> Vec<Chapter> {
        self.chapter_repo
            .find_by_file_id(self.file(rel).id)
            .await
            .unwrap()
    }

    async fn subtitles_of(&self, rel: &str) -> Vec<SidecarSubtitle> {
        self.sidecar_repo
            .find_by_file_id(self.file(rel).id)
            .await
            .unwrap()
    }

    /// The pin of every movie, sorted.
    fn movie_pins(&self) -> Vec<Option<String>> {
        let mut pins: Vec<Option<String>> = self
            .movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
            .map(|m| m.pinned_ref.clone())
            .collect();
        pins.sort();
        pins
    }

    /// The pin of every show, sorted.
    fn show_pins(&self) -> Vec<Option<String>> {
        let mut pins: Vec<Option<String>> = self
            .show_repo
            .shows
            .lock()
            .unwrap()
            .values()
            .map(|s| s.pinned_ref.clone())
            .collect();
        pins.sort();
        pins
    }

    async fn applied(&self, rel: &str) -> Option<AppliedNfo> {
        self.applied_nfo_repo
            .find_by_path(&self.root.join(rel))
            .await
            .unwrap()
    }

    /// Set the modification time of the file at `rel`, as `cp -p`, `rsync
    /// -a` or `touch -d` would.
    fn set_mtime(&self, rel: &str, mtime: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(self.root.join(rel))
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    async fn warnings(&self) -> Vec<String> {
        self.admin_log_repo
            .list(1000, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|log| log.level == AdminLogLevel::Warning)
            .map(|log| log.message)
            .collect()
    }
}

const MATRIX_NFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
  <title>The Matrix</title>
  <year>1999</year>
  <uniqueid type="imdb">tt0133093</uniqueid>
  <uniqueid type="tmdb" default="true">603</uniqueid>
</movie>"#;

fn tmdb_movie(id: u32) -> String {
    format!(r#"<movie><uniqueid type="tmdb">{id}</uniqueid></movie>"#)
}

/// Move `from` to `to` beneath the harness's library root, creating the
/// folders `to` needs.
fn move_file(h: &Harness, from: &str, to: &str) {
    let to = h.root.join(to);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(h.root.join(from), to).unwrap();
}

/// A movie folder -- video, subtitle and NFO -- indexed, then moved to a new
/// folder.
async fn a_moved_movie_folder() -> (Harness, Uuid) {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.en.srt", "1");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.scan().await;
    let id = h.file("Heat/Heat.mkv").id;
    assert_eq!(h.subtitles_of("Heat/Heat.mkv").await.len(), 1);
    assert!(h.applied("Heat/Heat.nfo").await.is_some());

    for name in ["Heat.mkv", "Heat.en.srt", "Heat.nfo"] {
        move_file(&h, &format!("Heat/{name}"), &format!("Heat (1995)/{name}"));
    }
    (h, id)
}

/// NFOs (issues #184, #185): what an NFO beside a video pins, how an edit,
/// a move or an administrator's match changes that, and what is never read.
#[path = "index_nfo_tests.rs"]
mod nfo;

/// Subtitles beside a video, indexed as its sidecars.
#[path = "index_sidecar_tests.rs"]
mod sidecar;

/// What a probe reads from the container -- its tags and chapters -- and
/// the invariant that indexing never writes into the library root.
#[path = "index_container_tests.rs"]
mod container;
