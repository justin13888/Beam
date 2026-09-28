//! What a scan and the watcher make of the metadata a library keeps beside
//! its media (issue #184): Kodi NFOs, container tags and sidecar subtitles --
//! and that reading them never writes a byte into the library.
//!
//! These run the real scan over a `TempDir` library, with in-memory doubles
//! below the repository traits.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;
use crate::probe::metadata::MetadataError;
use crate::services::admin_log::LocalAdminLogService;
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
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    enrichment_repo: Arc<InMemoryEnrichmentStateRepository>,
    sidecar_repo: Arc<InMemorySidecarSubtitleRepository>,
    applied_nfo_repo: Arc<InMemoryAppliedNfoRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    file_reads: Arc<CountingFileRepository>,
    tags: Tags,
    /// While set, every probe fails, as one of a file still being muxed.
    probe_fails: Arc<AtomicBool>,
    service: Arc<LocalIndexService>,
}

impl Harness {
    async fn new() -> Self {
        Self::build(Probe::Double, Arc::new(RealClock)).await
    }

    async fn with_real_prober() -> Self {
        let _ = crate::probe::init();
        Self::build(Probe::Real, Arc::new(RealClock)).await
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
        let applied_nfo_repo = Arc::new(InMemoryAppliedNfoRepository::default());
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
        let probe_fails = Arc::new(AtomicBool::new(false));
        let prober_fails = probe_fails.clone();
        let mut prober = MockMediaInfoService::new();
        prober.expect_get_video_metadata().returning(move |path| {
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
            library_repo,
            file_reads.clone(),
            movie_repo.clone(),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            hasher,
            prober,
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        )
        .with_enrichment_repo(enrichment_repo.clone())
        .with_sidecar_repo(sidecar_repo.clone())
        .with_applied_nfo_repo(applied_nfo_repo.clone())
        .with_clock(clock);

        Self {
            _dir: dir,
            root,
            library,
            file_repo,
            movie_repo,
            show_repo,
            enrichment_repo,
            sidecar_repo,
            applied_nfo_repo,
            admin_log_repo,
            file_reads,
            tags,
            probe_fails,
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
        self.service
            .scan_library(self.library.id.to_string())
            .await
            .unwrap();
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
        let Some(MediaFileContent::Movie { movie_entry_id }) = self.file(rel).content else {
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

#[tokio::test]
async fn an_nfo_pins_the_new_movie_shows_its_title_and_queues_a_fetch_by_the_pin() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.1080p.mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:603"));
    assert_eq!(movie.title, "The Matrix", "the NFO names the new movie");
    assert_eq!(movie.year, Some(1999));
    assert_eq!(
        movie.identity_key.as_deref(),
        Some("matrix|"),
        "the key is the path's"
    );
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh, "the pinned movie is fetched afresh");
    assert_eq!(row.matched_ref, None, "by its pin, not an old match");
}

#[tokio::test]
async fn a_second_file_whose_nfo_names_the_pin_joins_the_pinned_movie() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Elsewhere/The Matrix 4K.mkv");
    h.write("Elsewhere/The Matrix 4K.nfo", MATRIX_NFO);

    h.scan().await;

    assert_eq!(
        h.movie_of("Matrix/matrix.1080p.mkv").id,
        h.movie_of("Elsewhere/The Matrix 4K.mkv").id,
        "two paths keyed differently, one pinned title"
    );
    assert_eq!(h.movie_repo.movies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_title_keeps_its_pin_when_another_nfo_names_another_id() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write(
        "Heat (1995)/Heat (1995).nfo",
        r#"<movie><uniqueid type="tmdb">949</uniqueid></movie>"#,
    );
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write(
        "Heat (1995)/Heat (1995) - Remux.nfo",
        r#"<movie><uniqueid type="tmdb">1</uniqueid></movie>"#,
    );

    h.scan().await;

    let pins = h.movie_pins();
    assert_eq!(pins.len(), 1, "one key, one movie: {pins:?}");
    assert!(pins[0].is_some());
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("already pinned")),
        "the administrator is told the NFOs disagree"
    );
}

#[tokio::test]
async fn an_episode_nfo_makes_an_episode_of_a_movie_looking_name() {
    let h = Harness::new().await;
    h.video("Lost/Pilot Part One.mkv");
    h.write(
        "Lost/Pilot Part One.nfo",
        "<episodedetails><title>Pilot (1)</title><season>1</season><episode>1</episode>\
         </episodedetails>",
    );

    h.scan().await;

    let (show, season, episode) = h.show_of("Lost/Pilot Part One.mkv");
    assert_eq!((season, episode), (1, 1));
    assert_eq!(show.identity_key.as_deref(), Some("lost|"));
    assert!(h.movie_repo.movies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tvshow_nfo_names_and_pins_the_show_its_folder_keys() {
    let h = Harness::new().await;
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.write(
        "GoT/tvshow.nfo",
        r#"<tvshow><title>Game of Thrones</title><year>2011</year>
           <uniqueid type="tmdb" default="true">1399</uniqueid></tvshow>"#,
    );

    h.scan().await;

    let (show, _, _) = h.show_of("GoT/Season 01/GoT.S01E01.mkv");
    assert_eq!(show.title, "Game of Thrones");
    assert_eq!(show.year, Some(2011));
    assert_eq!(show.identity_key.as_deref(), Some("got|"));
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:1399"));
}

#[tokio::test]
async fn container_tags_place_an_episode_the_path_could_not() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );

    h.scan().await;

    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.identity_key.as_deref(), Some("the office|"));
}

/// A file placed only by its container tags keeps its place when a
/// classifier-version bump reclassifies it: its probe stored the tags with
/// it, and the reclassification reads them rather than probing again.
#[tokio::test]
async fn a_file_placed_by_its_tags_keeps_its_place_when_it_is_reclassified() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );
    h.scan().await;
    let (placed, _, _) = h.show_of("The Office/The Dundies.m4v");

    // As the next classifier version would find the row; and a re-probe
    // would no longer see the tags, so only the stored ones can place it.
    h.tags.lock().unwrap().clear();
    let id = h.file("The Office/The Dundies.m4v").id;
    h.file_repo
        .files
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .classifier_version = CLASSIFIER_VERSION - 1;
    h.scan().await;

    let file = h.file("The Office/The Dundies.m4v");
    assert_eq!(file.classifier_version, CLASSIFIER_VERSION, "reclassified");
    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.id, placed.id, "the same show as before");
}

/// A text tag longer than Beam keeps is stored cut to the cap, on a character
/// boundary: one file cannot store an unbounded value with its row.
#[tokio::test]
async fn a_container_tag_longer_than_the_cap_is_stored_cut_to_it() {
    use beam_domain::utils::classification::MAX_TAG_VALUE_BYTES;

    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    // Three-byte characters, so the cap falls inside one.
    let show = "\u{4e2d}".repeat(MAX_TAG_VALUE_BYTES);
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", show.as_str()),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );

    h.scan().await;

    let stored = h
        .file("The Office/The Dundies.m4v")
        .container_tags
        .expect("the probe's tags are stored")
        .show
        .expect("the show tag is kept");
    assert_eq!(
        stored,
        "\u{4e2d}".repeat(MAX_TAG_VALUE_BYTES / 3),
        "cut to the whole characters that fit"
    );
}

/// A file whose first probe failed is classified when a later probe succeeds
/// (issue #181) -- and by the tags that probe read, exactly as a new file
/// would be, not by its path alone.
#[tokio::test]
async fn a_reprobe_that_succeeds_classifies_by_the_tags_it_read() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );
    h.probe_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.scan().await;
    assert_eq!(h.file("The Office/The Dundies.m4v").content, None);

    h.probe_fails
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.scan().await;

    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.identity_key.as_deref(), Some("the office|"));
}

#[tokio::test]
async fn an_nfo_larger_than_beam_reads_is_ignored() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let padding = " ".repeat(beam_domain::utils::nfo::MAX_NFO_BYTES as usize);
    h.write("Matrix/movie.nfo", &format!("{MATRIX_NFO}{padding}"));

    h.scan().await;

    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);
}

#[cfg(unix)]
#[tokio::test]
async fn an_nfo_that_is_a_symlink_is_not_read() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let outside = h._dir.path().join("outside.nfo");
    std::fs::write(&outside, MATRIX_NFO).unwrap();
    std::os::unix::fs::symlink(&outside, h.root.join("Matrix/movie.nfo")).unwrap();

    h.scan().await;

    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);
}

/// An NFO added after its video was indexed pins the title at the next scan
/// -- even one copied in with its old modification time kept (`cp -p`,
/// `rsync -a`), or from a NAS whose clock runs behind the server's: whether
/// it is applied turns on what it holds, never on when it says it was
/// written.
/// The open itself refuses a link, not just the `lstat` before it: a link
/// swapped in between the two fails to open rather than being followed out
/// of the library.
#[cfg(unix)]
#[test]
fn an_nfo_is_opened_without_following_a_link() {
    let dir = TempDir::new().unwrap();
    let outside = dir.path().join("outside.nfo");
    std::fs::write(&outside, MATRIX_NFO).unwrap();
    let link = dir.path().join("movie.nfo");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    assert!(hints::open_no_follow(&outside).is_ok());
    assert!(hints::open_no_follow(&link).is_err());
}

/// A FIFO swapped in for an NFO opens at once rather than blocking the scan
/// until something writes to it -- and is then no regular file, so it is not
/// read.
#[cfg(target_os = "linux")]
#[test]
fn an_nfo_that_is_a_fifo_opens_without_blocking() {
    let dir = TempDir::new().unwrap();
    let fifo = dir.path().join("movie.nfo");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();

    let (sent, opened) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let is_file = hints::open_no_follow(&fifo).map(|file| file.metadata().unwrap().is_file());
        let _ = sent.send(is_file.ok());
    });
    let is_file = opened
        .recv_timeout(Duration::from_secs(10))
        .expect("the open returned instead of waiting for a writer");
    assert_eq!(is_file, Some(false), "opened, and seen not to be a file");
}

#[tokio::test]
async fn an_nfo_added_after_indexing_pins_the_movie_on_the_next_scan() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.scan().await;
    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);

    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.set_mtime(
        "Matrix/movie.nfo",
        std::time::SystemTime::now() - Duration::from_secs(365 * 24 * 3600),
    );
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:603"));
    assert!(
        h.enrichment_of(EnrichmentTargetId::Movie(movie.id))
            .await
            .force_refresh
    );
}

#[tokio::test]
async fn an_edited_nfo_repins_its_movie_when_the_watcher_sees_it() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604"),
        "the edited NFO's id replaces the one it set"
    );
}

/// A row a Postgres `FileRepository` returns holds its file's mtime in whole
/// microseconds, while the filesystem reports nanoseconds (issue #229). The
/// video beside an edited NFO is unmoved, so the NFO still re-pins on its
/// watcher event rather than being left to the next scan as though the video
/// might have moved.
#[tokio::test]
async fn an_edited_nfo_repins_on_its_event_beside_a_row_kept_to_microseconds() {
    use chrono::Timelike;

    let h = Harness::new().await;
    let video = h.video("Matrix/matrix.mkv");
    h.set_mtime(
        "Matrix/matrix.mkv",
        std::time::UNIX_EPOCH + Duration::new(1_790_000_000, 123_456_789),
    );
    let on_disk: DateTime<Utc> = std::fs::metadata(&video)
        .unwrap()
        .modified()
        .unwrap()
        .into();
    assert_eq!(
        on_disk.nanosecond(),
        123_456_789,
        "the filesystem keeps the sub-microsecond part, or this proves nothing"
    );
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;

    // The row exactly as `files.mtime`, a TIMESTAMPTZ, reads back.
    let at_micros = on_disk.with_nanosecond(123_456_000).unwrap();
    let row = h
        .file_repo
        .find_by_path(&video.to_string_lossy())
        .await
        .unwrap()
        .expect("the video is indexed");
    h.file_repo
        .update(UpdateMediaFile {
            id: row.id,
            hash: None,
            size_bytes: None,
            mtime: Some(at_micros),
            probe: ProbeUpdate::Keep,
            content: None,
            status: None,
        })
        .await
        .unwrap();

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604"),
        "the edit re-pins on its event, not at the next scan"
    );
}

/// An administrator's pin is a decision made in Beam: no NFO -- edited, or
/// named by a new file -- replaces it (FR-312).
#[tokio::test]
async fn an_nfo_never_replaces_an_administrators_pin() {
    use beam_domain::models::PinSource;
    use beam_domain::models::ProviderPin;
    use beam_domain::repositories::MovieRepository;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert!(
        h.movie_repo
            .set_pinned_ref(movie.id, &ProviderPin::Tmdb(624860), PinSource::Admin)
            .await
            .unwrap()
    );

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:624860"), Some(PinSource::Admin)),
        "the administrator's pin stands"
    );
}

/// The administrator's control of enrichment (issue #185) over this harness's
/// indexer, which re-pins a title by its NFO once its administrator's pin is
/// cleared.
fn enrichment_control(h: &Harness) -> crate::services::enrichment::control::EnrichmentControl {
    use crate::services::enrichment::control::{EnrichmentControl, EnrichmentControlDeps};
    use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;

    EnrichmentControl::new(EnrichmentControlDeps {
        movies: h.movie_repo.clone(),
        shows: h.show_repo.clone(),
        states: h.enrichment_repo.clone(),
        libraries: Arc::new(InMemoryLibraryRepository::default()),
        provider: Arc::new(InMemoryEnrichmentProvider::new(&["tmdb"])),
        admin_log: Arc::new(LocalAdminLogService::new(
            h.admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        )),
        nfo_pins: h.service.clone(),
        worker: Arc::new(tokio::sync::Notify::new()),
    })
}

/// An administrator's fix-match is an administrator's pin (FR-312): it
/// outranks the NFO's, and neither an edit of the NFO nor a rescan replaces
/// it.
#[tokio::test]
async fn a_fixed_match_outranks_the_nfo_and_survives_its_edit_and_a_rescan() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(movie.pin_source, Some(PinSource::Nfo));

    enrichment_control(&h)
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:624860"), Some(PinSource::Admin)),
        "the fixed match stands"
    );
}

/// Clearing an administrator's fix-match hands the title back to its NFO:
/// pinned by what the NFO says now, and queued to be fetched by that pin.
#[tokio::test]
async fn clearing_a_fixed_match_pins_the_title_by_its_nfo_again() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(movie.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo)),
        "the NFO's pin again"
    );
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh);
    assert_eq!(
        row.matched_ref, None,
        "fetched by the NFO's pin, not a match"
    );
}

/// With no NFO to fall back to, a cleared title is found by its path again:
/// unpinned, and queued to be searched for.
#[tokio::test]
async fn clearing_a_fixed_match_of_a_title_with_no_nfo_unpins_it() {
    let h = Harness::new().await;
    h.video("Shogun/Shogun.S01E01.mkv");
    h.scan().await;
    let (show, _, _) = h.show_of("Shogun/Shogun.S01E01.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(show.id, "tmdb:126308", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(show.id, "admin").await.unwrap();

    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));
    assert!(h.show_pins().iter().all(Option::is_none));
}

/// Clearing a fixed match while the NFO cannot be read -- a share offline
/// -- leaves the title unpinned, and the NFO forgotten as applied: when it
/// can be read again, the next scan pins the title by it, though its content
/// never changed.
#[tokio::test]
async fn an_nfo_unreadable_when_a_match_is_cleared_is_applied_when_it_returns() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let nfo = h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    let away = h.root.join("matrix.nfo.offline");
    std::fs::rename(&nfo, &away).unwrap();

    let detail = control.clear_match(movie.id, "admin").await.unwrap();
    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));

    std::fs::rename(&away, &nfo).unwrap();
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo)),
        "the NFO is applied once it can be read"
    );
}

/// An NFO edited while an administrator's pin held the title is what the
/// title goes back to when the pin is cleared: the edited id, not the one
/// the NFO held before.
#[tokio::test]
async fn clearing_a_fixed_match_takes_the_id_the_nfo_was_edited_to_meanwhile() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    let detail = control.clear_match(movie.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:604"), Some(PinSource::Nfo))
    );
}

/// Clearing a fixed match whose NFO names an id another title holds leaves
/// the title unpinned and the NFO forgotten as applied, so once the other
/// title lets the id go, the next scan pins the title by its NFO.
#[tokio::test]
async fn an_nfo_refused_when_a_match_is_cleared_is_applied_once_its_id_is_free() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/heat.mkv");
    h.scan().await;
    let matrix = h.movie_of("Matrix/matrix.mkv");
    let heat = h.movie_of("Heat/heat.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(matrix.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    control
        .fix_match(heat.id, "tmdb:603", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(matrix.id, "admin").await.unwrap();
    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));

    control
        .fix_match(heat.id, "tmdb:949", "admin")
        .await
        .unwrap();
    h.scan().await;

    let matrix = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (matrix.pinned_ref.as_deref(), matrix.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo))
    );
}

/// A show's `tvshow.nfo` is the one a cleared show goes back to.
#[tokio::test]
async fn clearing_a_shows_fixed_match_pins_it_by_its_tvshow_nfo() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("The Office/Season 02/The.Office.S02E01.mkv");
    h.write(
        "The Office/tvshow.nfo",
        r#"<tvshow><uniqueid type="tmdb">2316</uniqueid></tvshow>"#,
    );
    h.scan().await;
    let (show, _, _) = h.show_of("The Office/Season 02/The.Office.S02E01.mkv");
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:2316"));
    let control = enrichment_control(&h);
    control
        .fix_match(show.id, "tmdb:9999", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(show.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:2316"), Some(PinSource::Nfo))
    );
}

/// An NFO edited while a scan holds the library is not read under it: the
/// event is handed back to retry (issue #181), and the retry -- once the
/// library is free -- applies the edit rather than losing it.
#[tokio::test]
async fn an_nfo_event_while_the_library_is_held_is_deferred_and_applied_on_retry() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );

    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert_eq!(
        h.event_outcome("Matrix/movie.nfo", FsEventKind::Modified)
            .await,
        ReconcileOutcome::Deferred {
            retry_after: LIBRARY_BUSY_RETRY
        }
    );
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603"),
        "nothing is re-pinned while the library is held"
    );

    drop(held);
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
}

/// A subtitle added or removed while a scan holds the library is deferred the
/// same way, and recorded or forgotten by the retry.
#[tokio::test]
async fn a_subtitle_event_while_the_library_is_held_is_deferred_and_applied_on_retry() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.scan().await;
    h.write("Movie/Movie.fr.srt", "1");

    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert_eq!(
        h.event_outcome("Movie/Movie.fr.srt", FsEventKind::Created)
            .await,
        ReconcileOutcome::Deferred {
            retry_after: LIBRARY_BUSY_RETRY
        }
    );
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
    drop(held);
    h.event("Movie/Movie.fr.srt", FsEventKind::Created).await;
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);

    std::fs::remove_file(h.root.join("Movie/Movie.fr.srt")).unwrap();
    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert!(matches!(
        h.event_outcome("Movie/Movie.fr.srt", FsEventKind::Removed)
            .await,
        ReconcileOutcome::Deferred { .. }
    ));
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);
    drop(held);
    h.event("Movie/Movie.fr.srt", FsEventKind::Removed).await;
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
}

#[tokio::test]
async fn a_row_from_older_rules_is_reclassified_by_the_nfo_beside_it() {
    let h = Harness::new().await;
    h.video("Lost/Pilot Part One.mkv");
    h.scan().await;
    assert!(matches!(
        h.file("Lost/Pilot Part One.mkv").content,
        Some(MediaFileContent::Movie { .. })
    ));

    // As a build before this NFO support left it.
    let id = h.file("Lost/Pilot Part One.mkv").id;
    h.file_repo
        .files
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .classifier_version = CLASSIFIER_VERSION - 1;
    h.write(
        "Lost/Pilot Part One.nfo",
        "<episodedetails><season>1</season><episode>1</episode></episodedetails>",
    );
    h.scan().await;

    let file = h.file("Lost/Pilot Part One.mkv");
    assert_eq!(file.id, id, "reclassified in place");
    assert_eq!(file.classifier_version, CLASSIFIER_VERSION);
    assert!(matches!(
        file.content,
        Some(MediaFileContent::Episode { .. })
    ));
}

#[tokio::test]
async fn subtitles_beside_a_video_are_indexed_as_its_subtitles() {
    let h = Harness::new().await;
    h.video("Movie (2000)/Movie (2000).mkv");
    h.write("Movie (2000)/Movie (2000).en.forced.srt", "1");
    h.write("Movie (2000)/Movie (2000).pt-BR.vtt", "2");
    h.write("Movie (2000)/Subs/English.SDH.ass", "3");
    h.write("Movie (2000)/Other.en.srt", "no video of that name");
    h.write("Movie (2000)/Movie (2000).en.sup", "image-based");

    h.scan().await;

    let subtitles = h.subtitles_of("Movie (2000)/Movie (2000).mkv").await;
    let seen: Vec<(String, SubtitleFormat, Option<String>, bool, bool)> = subtitles
        .iter()
        .map(|s| {
            (
                s.path
                    .strip_prefix(&h.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                s.info.format,
                s.info.language.clone(),
                s.info.is_forced,
                s.info.is_sdh,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                "Movie (2000)/Movie (2000).en.forced.srt".to_string(),
                SubtitleFormat::Srt,
                Some("eng".to_string()),
                true,
                false
            ),
            (
                "Movie (2000)/Movie (2000).pt-BR.vtt".to_string(),
                SubtitleFormat::Vtt,
                Some("por".to_string()),
                false,
                false
            ),
            (
                "Movie (2000)/Subs/English.SDH.ass".to_string(),
                SubtitleFormat::Ass,
                Some("eng".to_string()),
                false,
                true
            ),
        ]
    );
    assert_eq!(
        h.sidecar_repo.rows.lock().unwrap().len(),
        3,
        "a subtitle no video owns is not indexed"
    );
    let forced = &subtitles[0];
    assert_eq!(forced.size_bytes, 1);
}

#[tokio::test]
async fn a_rescan_updates_a_changed_subtitle_and_deletes_a_vanished_one() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.write("Movie/Movie.en.srt", "1");
    h.write("Movie/Movie.de.srt", "2");
    h.scan().await;
    let before = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(before.len(), 2);

    std::fs::remove_file(h.root.join("Movie/Movie.de.srt")).unwrap();
    h.write("Movie/Movie.en.srt", "a longer subtitle");
    h.scan().await;

    let after = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(after.len(), 1);
    let english = before
        .iter()
        .find(|s| s.path.ends_with("Movie.en.srt"))
        .unwrap();
    assert_eq!(after[0].id, english.id, "updated in place");
    assert_eq!(after[0].size_bytes, "a longer subtitle".len() as u64);
}

#[tokio::test]
async fn the_watcher_indexes_a_new_subtitle_and_forgets_a_removed_one() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.scan().await;

    h.write("Movie/Movie.fr.srt", "1");
    h.event("Movie/Movie.fr.srt", FsEventKind::Created).await;
    let added = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].info.language.as_deref(), Some("fre"));

    std::fs::remove_file(h.root.join("Movie/Movie.fr.srt")).unwrap();
    h.event("Movie/Movie.fr.srt", FsEventKind::Removed).await;
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
}

#[tokio::test]
async fn a_subtitle_for_a_video_not_yet_indexed_waits_for_the_video() {
    let h = Harness::new().await;
    h.write("Movie/Movie.en.srt", "1");
    h.event("Movie/Movie.en.srt", FsEventKind::Created).await;
    assert!(h.sidecar_repo.rows.lock().unwrap().is_empty());

    h.video("Movie/Movie.mkv");
    h.event("Movie/Movie.mkv", FsEventKind::Created).await;
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);
}

/// A row keeps a subtitle's mtime in whole microseconds and the filesystem
/// reports nanoseconds (issue #229): a rescan still finds an untouched
/// subtitle unchanged, and writes nothing for it.
#[tokio::test]
async fn a_rescan_writes_nothing_for_a_subtitle_with_a_sub_microsecond_mtime() {
    use chrono::Timelike;

    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    let subtitle = h.write("Movie/Movie.fr.srt", "1");
    h.set_mtime(
        "Movie/Movie.fr.srt",
        std::time::UNIX_EPOCH + Duration::new(1_790_000_000, 123_456_789),
    );
    let on_disk: DateTime<Utc> = std::fs::metadata(&subtitle)
        .unwrap()
        .modified()
        .unwrap()
        .into();
    assert_eq!(
        on_disk.nanosecond(),
        123_456_789,
        "the filesystem keeps the sub-microsecond part, or this proves nothing"
    );
    h.scan().await;

    // Mark the row, so a rewrite -- which stamps it afresh -- shows.
    let marker = DateTime::<Utc>::UNIX_EPOCH;
    h.sidecar_repo
        .rows
        .lock()
        .unwrap()
        .get_mut(&subtitle)
        .expect("the subtitle is recorded")
        .updated_at = marker;
    h.scan().await;

    let rows = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].updated_at, marker,
        "the unchanged row is not rewritten"
    );
}

/// Every regular file under `root`, with its bytes, modification time and
/// permissions; and every directory, with its modification time.
fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>, std::time::SystemTime, bool)> {
    let mut entries: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            let bytes = meta.is_file().then(|| std::fs::read(entry.path()).unwrap());
            (
                entry.path().to_path_buf(),
                bytes,
                meta.modified().unwrap(),
                meta.permissions().readonly(),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// A library with everything the scan reads beside the media.
fn furnished(h: &Harness) {
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.write("Matrix/matrix.1080p.en.srt", "1");
    h.write("Matrix/Subs/French.srt", "2");
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.write(
        "GoT/tvshow.nfo",
        "<tvshow><title>Game of Thrones</title></tvshow>",
    );
    h.write(
        "GoT/Season 01/GoT.S01E01.nfo",
        "<episodedetails><season>1</season><episode>1</episode></episodedetails>",
    );
    h.write("GoT/Season 01/GoT.S01E01.eng.forced.ass", "3");
}

/// Everything the indexer does with a furnished library: the watcher
/// indexes a new video -- attaching the subtitles beside it and in its
/// `Subs/` folder -- then two scans, and watcher events for an NFO and a
/// subtitle.
async fn index_furnished(h: &Harness) {
    h.event("Matrix/matrix.1080p.mkv", FsEventKind::Created)
        .await;
    h.scan().await;
    h.scan().await;
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.event("Matrix/matrix.1080p.en.srt", FsEventKind::Modified)
        .await;
}

async fn assert_furnished_indexed(h: &Harness) {
    assert_eq!(
        h.movie_of("Matrix/matrix.1080p.mkv").pinned_ref.as_deref(),
        Some("tmdb:603")
    );
    assert_eq!(h.subtitles_of("Matrix/matrix.1080p.mkv").await.len(), 2);
    assert_eq!(
        h.subtitles_of("GoT/Season 01/GoT.S01E01.mkv").await.len(),
        1
    );
    assert_eq!(
        h.show_of("GoT/Season 01/GoT.S01E01.mkv").0.title,
        "Game of Thrones"
    );
}

/// A library holding a real container -- opened by the real FFmpeg prober,
/// tags and all, and hashed by the real hasher -- with an NFO and a subtitle
/// in a `Subs/` folder beside it.
fn furnished_with_a_real_container(h: &Harness) {
    let clip = h.root.join("Clip (2020)/Clip (2020).mkv");
    std::fs::create_dir_all(clip.parent().unwrap()).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/h264.mkv"),
        &clip,
    )
    .unwrap();
    h.write(
        "Clip (2020)/Clip (2020).nfo",
        r#"<movie><uniqueid type="tmdb">949</uniqueid></movie>"#,
    );
    h.write("Clip (2020)/Subs/English.srt", "1");
}

async fn index_the_real_container(h: &Harness) {
    h.event("Clip (2020)/Clip (2020).mkv", FsEventKind::Created)
        .await;
    h.scan().await;
    h.scan().await;
    h.event("Clip (2020)/Clip (2020).nfo", FsEventKind::Modified)
        .await;
}

async fn assert_the_real_container_indexed(h: &Harness) {
    let file = h.file("Clip (2020)/Clip (2020).mkv");
    assert_eq!(file.status, FileStatus::Known, "probed by the real prober");
    assert!(file.duration.is_some());
    assert_eq!(
        h.movie_of("Clip (2020)/Clip (2020).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949")
    );
    assert_eq!(h.subtitles_of("Clip (2020)/Clip (2020).mkv").await.len(), 1);
}

#[tokio::test]
async fn a_scan_never_writes_into_the_library_root() {
    let h = Harness::new().await;
    furnished(&h);
    let before = snapshot(&h.root);

    index_furnished(&h).await;

    assert_furnished_indexed(&h).await;
    assert_eq!(
        snapshot(&h.root),
        before,
        "the library tree is byte-for-byte unchanged"
    );
}

/// The real prober opens the container to read its streams and tags, and
/// the real hasher reads it whole: neither writes a byte either.
#[tokio::test]
async fn probing_and_hashing_a_real_container_never_write_into_the_library_root() {
    let h = Harness::with_real_prober().await;
    furnished_with_a_real_container(&h);
    let before = snapshot(&h.root);

    index_the_real_container(&h).await;

    assert_the_real_container_indexed(&h).await;
    assert_eq!(
        snapshot(&h.root),
        before,
        "the library tree is byte-for-byte unchanged"
    );
}

/// Run `indexing` with every write permission under `h`'s library removed,
/// restoring them after. `false` -- having run nothing, and said why -- when
/// the test runs as root, which ignores permissions, so the premise of a
/// read-only library does not hold.
#[cfg(unix)]
async fn with_the_library_read_only(h: &Harness, indexing: impl Future<Output = ()>) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let set_mode = |mode: u32| {
        for entry in walkdir::WalkDir::new(&h.root).contents_first(true) {
            let entry = entry.unwrap();
            let mode = if entry.file_type().is_dir() {
                mode | 0o111
            } else {
                mode
            };
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        }
    };
    set_mode(0o444);
    let root_user = std::fs::File::create(h.root.join("probe")).is_ok();
    if root_user {
        set_mode(0o755);
        eprintln!(
            "skipped: running as root, which ignores file permissions, so a read-only \
             library cannot be made"
        );
        return false;
    }
    indexing.await;
    set_mode(0o755);
    true
}

/// A read-only library -- a `:ro` mount, a share the server may not write --
/// indexes exactly as a writable one: nothing the scan does needs a write.
#[cfg(unix)]
#[tokio::test]
async fn a_read_only_library_is_indexed_in_full() {
    let h = Harness::new().await;
    furnished(&h);
    if with_the_library_read_only(&h, index_furnished(&h)).await {
        assert_furnished_indexed(&h).await;
    }
}

/// The same, with the real prober and hasher opening a real container.
#[cfg(unix)]
#[tokio::test]
async fn a_real_container_in_a_read_only_library_is_indexed_in_full() {
    let h = Harness::with_real_prober().await;
    furnished_with_a_real_container(&h);
    if with_the_library_read_only(&h, index_the_real_container(&h)).await {
        assert_the_real_container_indexed(&h).await;
    }
}

/// A video the watcher indexes picks up the subtitles already beside it --
/// in its folder and in a `Subs/` folder beside it.
#[tokio::test]
async fn a_video_the_watcher_indexes_picks_up_its_subtitles_and_its_subs_folder() {
    let h = Harness::new().await;
    h.write("Movie (2000)/Movie (2000).en.srt", "1");
    h.write("Movie (2000)/Subs/French.srt", "2");
    h.video("Movie (2000)/Movie (2000).mkv");

    h.event("Movie (2000)/Movie (2000).mkv", FsEventKind::Created)
        .await;

    let languages: Vec<Option<String>> = h
        .subtitles_of("Movie (2000)/Movie (2000).mkv")
        .await
        .into_iter()
        .map(|s| s.info.language)
        .collect();
    assert_eq!(
        languages,
        vec![Some("eng".to_string()), Some("fre".to_string())]
    );
}

fn tmdb_movie(id: u32) -> String {
    format!(r#"<movie><uniqueid type="tmdb">{id}</uniqueid></movie>"#)
}

fn tmdb_show(id: u32) -> String {
    format!(r#"<tvshow><uniqueid type="tmdb">{id}</uniqueid></tvshow>"#)
}

/// A second scan finds a new file whose own NFO names another id than the
/// title's pin. Classification keeps the pin and tells the administrator,
/// and records the NFO as applied, so the same scan's re-apply -- and every
/// later one, however the NFO's mtime moves -- does not then replace the
/// pin with it (FR-219).
#[tokio::test]
async fn a_new_file_whose_nfo_names_another_id_never_replaces_the_titles_pin() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;

    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995) - Remux.mkv").id,
        h.movie_of("Heat (1995)/Heat (1995).mkv").id,
        "one key, one movie"
    );
    assert_eq!(
        h.movie_pins(),
        vec![Some("tmdb:949".to_string())],
        "the title keeps the pin it had"
    );
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("tmdb:1,") && w.contains("already pinned to tmdb:949")),
        "the administrator is told which pin was kept: {:?}",
        h.warnings().await
    );

    h.set_mtime(
        "Heat (1995)/Heat (1995) - Remux.nfo",
        std::time::SystemTime::now() + Duration::from_secs(3600),
    );
    h.scan().await;
    assert_eq!(
        h.movie_pins(),
        vec![Some("tmdb:949".to_string())],
        "a newer mtime on unchanged content applies nothing"
    );
}

/// A `tvshow.nfo` at the library root describes no show -- not even one
/// whose episodes sit directly in a series folder below the root, as a
/// season folder's do below a series folder: neither a scan nor its watcher
/// event pins anything with it (FR-219).
#[tokio::test]
async fn a_tvshow_nfo_at_the_library_root_pins_no_show() {
    let h = Harness::new().await;
    h.video("Breaking Bad/Season 01/Breaking.Bad.S01E01.mkv");
    h.video("The Wire/The.Wire.S01E01.mkv");
    h.scan().await;

    h.write("tvshow.nfo", &tmdb_show(1396));
    h.scan().await;
    h.event("tvshow.nfo", FsEventKind::Modified).await;

    assert_eq!(h.show_pins(), vec![None, None]);
}

/// A `tvshow.nfo` describes the episodes in its folder and in the folders
/// one level below it -- a series folder's season folders -- and no deeper:
/// one in a category folder above the series folders pins nothing.
#[tokio::test]
async fn a_tvshow_nfo_two_levels_above_an_episode_pins_no_show() {
    let h = Harness::new().await;
    h.video("TV/Breaking Bad/Season 01/Breaking.Bad.S01E01.mkv");
    h.scan().await;

    h.write("TV/tvshow.nfo", &tmdb_show(1438));
    h.event("TV/tvshow.nfo", FsEventKind::Created).await;
    h.scan().await;

    assert_eq!(h.show_pins(), vec![None]);
}

/// A category folder above flat show folders -- `TV/Lost/Lost.S01E01.mkv` --
/// is not a series folder: the folder above an episode is looked in only when
/// the episode's own folder is a season folder. So a `tvshow.nfo` there,
/// found by the first scan, neither merges the shows beneath it into one
/// title nor pins any of them.
#[tokio::test]
async fn a_tvshow_nfo_in_a_category_folder_above_flat_shows_describes_none_of_them() {
    let h = Harness::new().await;
    h.video("TV/Lost/Lost.S01E01.mkv");
    h.video("TV/The Wire/The.Wire.S01E01.mkv");
    h.write("TV/tvshow.nfo", &tmdb_show(1438));

    h.scan().await;

    let (lost, _, _) = h.show_of("TV/Lost/Lost.S01E01.mkv");
    let (wire, _, _) = h.show_of("TV/The Wire/The.Wire.S01E01.mkv");
    assert_ne!(lost.id, wire.id, "two shows, not one");
    assert_eq!(lost.identity_key.as_deref(), Some("lost|"));
    assert_eq!(h.show_pins(), vec![None, None]);
}

/// The same category `tvshow.nfo`, added after the flat shows beneath it were
/// indexed, pins none of them -- by its watcher event or by a scan.
#[tokio::test]
async fn a_tvshow_nfo_added_to_a_category_folder_above_flat_shows_pins_none_of_them() {
    let h = Harness::new().await;
    h.video("TV/Lost/Lost.S01E01.mkv");
    h.video("TV/The Wire/The.Wire.S01E01.mkv");
    h.scan().await;

    h.write("TV/tvshow.nfo", &tmdb_show(1438));
    h.event("TV/tvshow.nfo", FsEventKind::Created).await;
    h.scan().await;

    assert_eq!(h.show_pins(), vec![None, None]);
}

/// `<stem>.nfo` wins over `movie.nfo` for the video it is named after, so a
/// `movie.nfo` added beside it later re-pins nothing -- by scan or watcher.
#[tokio::test]
async fn a_movie_nfo_never_overrides_a_videos_own_stem_nfo() {
    let h = Harness::new().await;
    h.video("Heat/Heat (1995).mkv");
    h.write("Heat/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;

    h.write("Heat/movie.nfo", &tmdb_movie(2));
    h.scan().await;
    h.event("Heat/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Heat/Heat (1995).mkv").pinned_ref.as_deref(),
        Some("tmdb:949")
    );
}

/// An NFO edited to an id another title holds is refused by the unique pin
/// and not recorded as applied, so once that title lets the id go the next
/// scan applies it.
#[tokio::test]
async fn an_nfo_whose_pin_another_title_holds_is_tried_again_until_it_applies() {
    let h = Harness::new().await;
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.video("Alien/Alien.mkv");
    h.write("Alien/Alien.nfo", &tmdb_movie(348));
    h.scan().await;
    let applied_before = h.applied("Alien/Alien.nfo").await.expect("recorded");

    h.write("Alien/Alien.nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(
        h.movie_of("Alien/Alien.mkv").pinned_ref.as_deref(),
        Some("tmdb:348"),
        "Heat holds tmdb:949"
    );
    assert_eq!(
        h.applied("Alien/Alien.nfo")
            .await
            .expect("still recorded")
            .content_hash,
        applied_before.content_hash,
        "a refused NFO is not recorded as applied"
    );

    h.write("Heat/Heat.nfo", &tmdb_movie(1));
    h.event("Heat/Heat.nfo", FsEventKind::Modified).await;
    assert_eq!(
        h.movie_of("Heat/Heat.mkv").pinned_ref.as_deref(),
        Some("tmdb:1")
    );
    h.scan().await;

    assert_eq!(
        h.movie_of("Alien/Alien.mkv").pinned_ref.as_deref(),
        Some("tmdb:949"),
        "tried again, and applied once the id was free"
    );
}

/// A harness whose clock runs an hour ahead of the files it writes, so a scan
/// records every NFO's stat stamp and a later scan with the same stamp reads
/// no NFO bytes.
async fn harness_ahead_of_its_files() -> Harness {
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now() + chrono::Duration::hours(1),
    ));
    Harness::build(Probe::Double, clock).await
}

/// The warnings that say an NFO describes several titles.
async fn several_titles_warnings(h: &Harness) -> Vec<String> {
    h.warnings()
        .await
        .into_iter()
        .filter(|w| w.contains("An NFO pins several"))
        .collect()
}

/// A `movie.nfo` added to a folder of two already-indexed movies can pin only
/// one of them: the other is refused by the unique pin because its sibling now
/// holds the id -- from this very NFO, so no retry could ever succeed. The NFO
/// is settled at once: recorded after one scan, never read again while
/// unchanged, and the administrator told exactly once.
#[cfg(unix)]
#[tokio::test]
async fn a_movie_nfo_describing_two_movies_is_settled_once_and_the_administrator_told() {
    let h = harness_ahead_of_its_files().await;
    h.video("Collection/Heat (1995).mkv");
    h.video("Collection/Alien (1979).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None, None], "two movies");

    h.write("Collection/movie.nfo", &tmdb_movie(949));
    h.scan().await;

    assert_eq!(
        h.movie_pins(),
        vec![None, Some("tmdb:949".to_string())],
        "an id pins one title"
    );
    let pinned = [
        h.movie_of("Collection/Heat (1995).mkv"),
        h.movie_of("Collection/Alien (1979).mkv"),
    ]
    .into_iter()
    .min_by_key(|movie| movie.id)
    .unwrap()
    .pinned_ref;
    assert_eq!(
        pinned.as_deref(),
        Some("tmdb:949"),
        "the title with the lowest id, the same one every time"
    );
    assert!(
        h.applied("Collection/movie.nfo").await.is_some(),
        "recorded as applied after one scan"
    );
    let warnings = several_titles_warnings(&h).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("movies") && warnings[0].contains("tmdb:949"));

    let before = nfo_reads();
    h.scan().await;
    h.scan().await;
    assert_eq!(nfo_reads(), before, "a settled NFO is not read again");
    assert_eq!(several_titles_warnings(&h).await.len(), 1, "told once");
    assert_eq!(h.movie_pins(), vec![None, Some("tmdb:949".to_string())]);
}

/// The same for a `tvshow.nfo` added to a flat show folder whose episodes key
/// to two shows.
#[cfg(unix)]
#[tokio::test]
async fn a_tvshow_nfo_describing_two_shows_is_settled_once_and_the_administrator_told() {
    let h = harness_ahead_of_its_files().await;
    h.video("Lost/Lost.S01E01.mkv");
    h.video("Lost/The.Wire.S01E01.mkv");
    h.scan().await;
    assert_eq!(h.show_pins(), vec![None, None], "two shows");

    h.write("Lost/tvshow.nfo", &tmdb_show(4607));
    h.scan().await;

    assert_eq!(
        h.show_pins(),
        vec![None, Some("tmdb:4607".to_string())],
        "an id pins one title"
    );
    assert!(
        h.applied("Lost/tvshow.nfo").await.is_some(),
        "recorded as applied after one scan"
    );
    let warnings = several_titles_warnings(&h).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("shows") && warnings[0].contains("tmdb:4607"));

    let before = nfo_reads();
    h.scan().await;
    h.scan().await;
    assert_eq!(nfo_reads(), before, "a settled NFO is not read again");
    assert_eq!(several_titles_warnings(&h).await.len(), 1, "told once");
}

/// An edited `<stem>.nfo` re-pins the movie of its own video at the next
/// scan.
#[tokio::test]
async fn an_edited_stem_nfo_repins_its_movie_at_the_next_scan() {
    let h = Harness::new().await;
    h.video("Heat/Heat (1995).mkv");
    h.write("Heat/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;

    h.write("Heat/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;

    let movie = h.movie_of("Heat/Heat (1995).mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:1"));
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh, "re-pinned, so fetched afresh");
    assert_eq!(row.matched_ref, None);
}

/// A `tvshow.nfo` added to a series folder after its episodes were indexed
/// pins their show at the next scan -- episodes in every season folder
/// below it -- and an edit of it the watcher sees re-pins the show.
#[tokio::test]
async fn a_tvshow_nfo_added_later_pins_its_show_and_an_edit_repins_it() {
    let h = Harness::new().await;
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.video("GoT/Season 02/GoT.S02E01.mkv");
    h.scan().await;
    assert_eq!(h.show_pins(), vec![None]);

    h.write("GoT/tvshow.nfo", &tmdb_show(1399));
    h.scan().await;
    let (show, _, _) = h.show_of("GoT/Season 02/GoT.S02E01.mkv");
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:1399"));
    assert!(
        h.enrichment_of(EnrichmentTargetId::Show(show.id))
            .await
            .force_refresh
    );

    h.write("GoT/tvshow.nfo", &tmdb_show(1400));
    h.event("GoT/tvshow.nfo", FsEventKind::Modified).await;
    assert_eq!(h.show_pins(), vec![Some("tmdb:1400".to_string())]);
}

/// An NFO edited before a scan that stamped its start and then died -- or
/// any scan whose start post-dates the edit -- is still applied by the next
/// scan: nothing turns on scan times.
#[tokio::test]
async fn an_nfo_edited_before_a_scan_that_died_is_applied_by_the_next() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.scan().await;
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    // As a scan that started after the edit and was killed would leave it.
    h.service
        .library_repo()
        .update_scan_progress(
            h.library.id,
            Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            None,
            None,
        )
        .await
        .unwrap();

    h.scan().await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603")
    );
}

/// An NFO whose stat stamp is the one recorded was not written since, and is
/// not read again; one with no recorded stamp is.
#[cfg(unix)]
#[tokio::test]
async fn an_nfo_whose_stat_stamp_is_unchanged_is_not_read_again() {
    use beam_domain::models::{PinSource, ProviderPin};
    use beam_domain::repositories::MovieRepository;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert!(
        h.movie_repo
            .set_pinned_ref(movie.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
            .await
            .unwrap()
    );
    // A record whose content no longer matches the NFO, but whose stamp
    // does: only a read would find the difference.
    let nfo = h.root.join("Matrix/movie.nfo");
    let stamp = hints::change_stamp(&std::fs::symlink_metadata(&nfo).unwrap());
    let stale = |stamp: Option<String>| {
        let mut rows = h.applied_nfo_repo.rows.lock().unwrap();
        let row = rows.get_mut(&nfo).unwrap();
        row.content_hash = "stale".to_string();
        row.change_stamp = stamp;
    };

    stale(stamp);
    h.scan().await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:1"),
        "not read, so not re-applied"
    );

    stale(None);
    h.scan().await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603"),
        "read, found changed, re-applied"
    );
}

/// An NFO written moments before it was read is recorded without its stat
/// stamp: a write within the same timestamp tick would not move the stamp,
/// so the next scan reads it again rather than trust it.
#[tokio::test]
async fn an_nfo_written_just_before_it_is_read_is_recorded_without_a_stamp() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let record = h
        .applied("Matrix/movie.nfo")
        .await
        .expect("classification recorded the NFO it read");
    assert_eq!(record.change_stamp, None);
    assert_eq!(record.size_bytes, MATRIX_NFO.len() as u64);
}

/// The record of an NFO that is deleted goes with it -- by the watcher, and
/// by the next scan -- so a record is kept only for an NFO on disk.
#[tokio::test]
async fn a_deleted_nfos_record_is_forgotten() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.scan().await;
    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert!(h.applied("Heat/Heat.nfo").await.is_some());

    std::fs::remove_file(h.root.join("Matrix/movie.nfo")).unwrap();
    h.event("Matrix/movie.nfo", FsEventKind::Removed).await;
    assert!(h.applied("Matrix/movie.nfo").await.is_none());

    std::fs::remove_file(h.root.join("Heat/Heat.nfo")).unwrap();
    h.scan().await;
    assert!(h.applied("Heat/Heat.nfo").await.is_none());
    assert_eq!(
        h.movie_of("Heat/Heat.mkv").pinned_ref.as_deref(),
        Some("tmdb:949"),
        "deleting an NFO leaves its pin"
    );
}

/// An NFO watcher event reads only the files beneath the NFO's folder -- one
/// query -- never the whole library's file list.
#[tokio::test]
async fn an_nfo_event_reads_only_the_files_beneath_its_folder() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/Heat.mkv");
    h.scan().await;
    h.file_reads
        .whole_library_reads
        .store(0, std::sync::atomic::Ordering::SeqCst);
    h.file_reads
        .folder_reads
        .store(0, std::sync::atomic::Ordering::SeqCst);

    h.write("Matrix/movie.nfo", &tmdb_movie(604));
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
    assert_eq!(
        (
            h.file_reads
                .whole_library_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            h.file_reads
                .folder_reads
                .load(std::sync::atomic::Ordering::SeqCst)
        ),
        (0, 1),
        "(whole-library reads, folder reads)"
    );
}

/// How many NFOs this test's thread has read the bytes of.
fn nfo_reads() -> usize {
    hints::NFO_READS.with(std::cell::Cell::get)
}

/// A scan well after an NFO's last write -- by the injected clock -- records
/// its stat stamp, and a later scan that finds the same stamp does not read
/// the NFO again; a write moves the stamp, and the next scan reads it.
#[cfg(unix)]
#[tokio::test]
async fn a_settled_nfos_stamp_is_recorded_and_spares_the_next_scan_a_read() {
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now() + chrono::Duration::hours(1),
    ));
    let h = Harness::build(Probe::Double, clock).await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let nfo = h.root.join("Matrix/movie.nfo");
    let stamp = hints::change_stamp(&std::fs::symlink_metadata(&nfo).unwrap());
    assert!(stamp.is_some());
    assert_eq!(
        h.applied("Matrix/movie.nfo")
            .await
            .expect("recorded")
            .change_stamp,
        stamp,
        "written long before the clock's now, so its stamp is vouched for"
    );

    let before = nfo_reads();
    h.scan().await;
    assert_eq!(nfo_reads(), before, "the same stamp: not read again");

    h.write("Matrix/movie.nfo", &tmdb_movie(604));
    h.scan().await;
    assert!(nfo_reads() > before, "a write moved the stamp: read");
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
}

/// A walk that cannot read the folder a kept, conflicting NFO lives in says
/// nothing about that NFO: its record is kept, so once the folder is readable
/// again the NFO is not taken for a new one and does not replace the pin.
#[cfg(unix)]
#[tokio::test]
async fn a_walk_failure_where_a_kept_nfo_lives_keeps_its_record_and_the_pin() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new().await;
    h.video("Alien (1979)/Alien (1979).mkv");
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949"),
        "the conflicting NFO was kept"
    );

    let locked = h.root.join("Heat (1995)");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not bind, so the walk cannot fail.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: running as root, which ignores file permissions");
        return;
    }
    let scanned = h.service.scan_library(h.library.id.to_string()).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    scanned.unwrap();

    assert!(
        h.applied("Heat (1995)/Heat (1995) - Remux.nfo")
            .await
            .is_some(),
        "an NFO the walk could not see is not an NFO the walk found gone"
    );
    h.scan().await;
    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949"),
        "the kept NFO is not re-applied as new"
    );
}

/// A removal the watcher reports while the library root is gone -- a volume
/// unmounted -- is not an NFO or a subtitle deleted: their records stay.
#[tokio::test]
async fn a_removal_while_the_root_is_gone_forgets_no_nfo_or_subtitle() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.write("Matrix/matrix.en.srt", "1");
    h.scan().await;
    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert_eq!(h.subtitles_of("Matrix/matrix.mkv").await.len(), 1);

    let parked = h.root.with_extension("unmounted");
    std::fs::rename(&h.root, &parked).unwrap();
    h.event("Matrix/movie.nfo", FsEventKind::Removed).await;
    h.event("Matrix/matrix.en.srt", FsEventKind::Removed).await;
    std::fs::rename(&parked, &h.root).unwrap();

    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert_eq!(h.subtitles_of("Matrix/matrix.mkv").await.len(), 1);
}

/// Deleting a kept, conflicting NFO forgets its record; one created at that
/// path again is an NFO added after indexing, and re-pins the title (FR-219).
#[tokio::test]
async fn a_kept_nfo_deleted_and_recreated_repins_its_title() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    std::fs::remove_file(h.root.join("Heat (1995)/Heat (1995) - Remux.nfo")).unwrap();
    h.event("Heat (1995)/Heat (1995) - Remux.nfo", FsEventKind::Removed)
        .await;
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.event("Heat (1995)/Heat (1995) - Remux.nfo", FsEventKind::Created)
        .await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:1".to_string())]);
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
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
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

/// A scan relinks a moved video to its row (issue #180) before it judges
/// what sits beside the media: the row's subtitles are those beside its new
/// path, and its NFO's record moves with the NFO, the title keeping its pin.
#[tokio::test]
async fn a_scan_relinks_a_moved_video_with_its_subtitles_and_nfo_by_path() {
    let (h, id) = a_moved_movie_folder().await;

    h.scan().await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitles: Vec<PathBuf> = h
        .subtitles_of("Heat (1995)/Heat.mkv")
        .await
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(subtitles, vec![h.root.join("Heat (1995)/Heat.en.srt")]);
    assert!(
        h.applied("Heat/Heat.nfo").await.is_none(),
        "the old NFO is gone"
    );
    assert!(h.applied("Heat (1995)/Heat.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// The watcher relinking a moved video, before it hears of the subtitle that
/// moved with it, records the subtitle beside the video's new path and
/// forgets the row of the one no longer beside it; the subtitle's and the
/// NFO's own events then change nothing further, and the title keeps its
/// pin.
#[tokio::test]
async fn the_watcher_relinks_a_moved_video_with_the_subtitles_beside_its_new_path() {
    let (h, id) = a_moved_movie_folder().await;

    h.event("Heat (1995)/Heat.mkv", FsEventKind::Created).await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitle_paths = || async {
        h.subtitles_of("Heat (1995)/Heat.mkv")
            .await
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<PathBuf>>()
    };
    assert_eq!(
        subtitle_paths().await,
        vec![h.root.join("Heat (1995)/Heat.en.srt")],
        "the subtitle beside the new path, and not the one gone from the old"
    );

    h.event("Heat/Heat.en.srt", FsEventKind::Removed).await;
    h.event("Heat (1995)/Heat.en.srt", FsEventKind::Created)
        .await;
    h.event("Heat/Heat.nfo", FsEventKind::Removed).await;
    h.event("Heat (1995)/Heat.nfo", FsEventKind::Created).await;

    assert_eq!(
        subtitle_paths().await,
        vec![h.root.join("Heat (1995)/Heat.en.srt")]
    );
    assert!(h.applied("Heat/Heat.nfo").await.is_none());
    assert!(h.applied("Heat (1995)/Heat.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// A movie folder renamed whole, which the watcher reports as the directory
/// alone: its event relinks the video, records the subtitle beside it in
/// place of the one at the old path, and applies the NFO it holds, the title
/// keeping its pin.
#[tokio::test]
async fn a_renamed_movie_folders_event_carries_its_subtitles_and_nfo_along() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.en.srt", "1");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.scan().await;
    let id = h.file("Heat/Heat.mkv").id;

    std::fs::rename(h.root.join("Heat"), h.root.join("Heat (1995)")).unwrap();
    h.event("Heat (1995)", FsEventKind::Created).await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitles: Vec<PathBuf> = h
        .subtitles_of("Heat (1995)/Heat.mkv")
        .await
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(subtitles, vec![h.root.join("Heat (1995)/Heat.en.srt")]);
    assert!(
        h.applied("Heat (1995)/Heat.nfo").await.is_some(),
        "the NFO inside is applied by the directory's event"
    );
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// Two copies of Heat in their own folders, indexed one scan after the other:
/// `One/`'s NFO pins the movie to tmdb:949, and `Two/`'s, naming tmdb:1, is
/// kept against it and recorded as applied (FR-219).
async fn a_kept_conflicting_nfo_in_its_own_folder() -> Harness {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("One/Heat (1995).mkv");
    h.write("One/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Two/Heat (1995).mkv");
    h.write("Two/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Two/Heat (1995).nfo").await.is_some());
    h
}

/// A scan relinks the video of a kept NFO's moved folder (issue #180) and
/// carries the NFO's applied state with it: moved, not edited, it is not an
/// NFO added after indexing, and the title keeps its pin.
#[tokio::test]
async fn a_kept_nfos_folder_moved_keeps_the_pin_through_a_scan() {
    let h = a_kept_conflicting_nfo_in_its_own_folder().await;
    let id = h.file("Two/Heat (1995).mkv").id;

    std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
    h.scan().await;

    assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
    assert!(h.applied("Two/Heat (1995).nfo").await.is_none());
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// The watcher reports a renamed folder as the new directory and the old
/// one, in either order. Either way the relinked video carries its kept
/// NFO's applied state: before the old folder's removal forgets its record,
/// by that record; after, as classification applies an NFO a title never
/// had -- keeping the pin. A scan later agrees.
#[tokio::test]
async fn a_kept_nfos_folder_moved_keeps_the_pin_through_directory_events_in_either_order() {
    for created_first in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;
        let id = h.file("Two/Heat (1995).mkv").id;

        std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
        if created_first {
            h.event("Moved", FsEventKind::Created).await;
            h.event("Two", FsEventKind::Removed).await;
        } else {
            h.event("Two", FsEventKind::Removed).await;
            h.event("Moved", FsEventKind::Created).await;
        }

        assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "created first: {created_first}"
        );
        assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
        assert!(h.applied("Two/Heat (1995).nfo").await.is_none());
        h.scan().await;
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    }
}

/// A kept NFO edited in the same move as its folder is an edited NFO: its
/// new id replaces the pin, by a scan and by the directory's event alike.
#[tokio::test]
async fn a_kept_nfo_edited_as_its_folder_moves_repins_its_title() {
    for by_scan in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;

        std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
        h.write("Moved/Heat (1995).nfo", &tmdb_movie(2));
        if by_scan {
            h.scan().await;
        } else {
            h.event("Moved", FsEventKind::Created).await;
            h.event("Two", FsEventKind::Removed).await;
        }

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "by scan: {by_scan}"
        );
    }
}

/// A kept show NFO above a season folder moves with its show's folder: the
/// relinked episode finds the show NFO it had above its old season folder
/// by its record, and the show keeps its pin.
#[tokio::test]
async fn a_kept_show_nfos_folder_moved_keeps_the_shows_pin() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("TV/Lost/Season 01/Lost - S01E01.mkv");
    h.write("TV/Lost/tvshow.nfo", &tmdb_show(4607));
    h.scan().await;
    h.video("Archive/Lost/Season 01/Lost - S01E02.mkv");
    h.write("Archive/Lost/tvshow.nfo", &tmdb_show(1));
    h.scan().await;
    assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
    let id = h.file("Archive/Lost/Season 01/Lost - S01E02.mkv").id;

    std::fs::rename(h.root.join("Archive"), h.root.join("Old TV")).unwrap();
    h.scan().await;

    assert_eq!(
        h.file("Old TV/Lost/Season 01/Lost - S01E02.mkv").id,
        id,
        "relinked"
    );
    assert!(h.applied("Old TV/Lost/tvshow.nfo").await.is_some());
    assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
}

/// A kept NFO whose folder Beam saw removed -- by the directory's own
/// removal event, or by a scan while it was missing -- is forgotten with it,
/// exactly as a removed NFO's own event forgets it: put back, it is an NFO
/// added after indexing, and re-pins its title (FR-219).
#[tokio::test]
async fn a_kept_nfos_folder_seen_removed_and_put_back_repins_its_title() {
    for by_scan in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;
        let away = h.root.parent().unwrap().join("Two, parked");

        std::fs::rename(h.root.join("Two"), &away).unwrap();
        if by_scan {
            h.scan().await;
        } else {
            h.event("Two", FsEventKind::Removed).await;
        }
        assert!(
            h.applied("Two/Heat (1995).nfo").await.is_none(),
            "forgotten with its folder, by scan: {by_scan}"
        );
        std::fs::rename(&away, h.root.join("Two")).unwrap();
        if by_scan {
            h.scan().await;
        } else {
            h.event("Two", FsEventKind::Created).await;
        }

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:1".to_string())],
            "by scan: {by_scan}"
        );
    }
}

/// A video moved into a folder whose NFO it never had -- its old folder held
/// none -- is pinned by that NFO as classification would pin a new file's,
/// and the NFO recorded as applied.
#[tokio::test]
async fn a_video_moved_beside_an_nfo_it_never_had_is_pinned_by_it() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("Incoming/Alien (1979).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None]);
    let id = h.file("Incoming/Alien (1979).mkv").id;

    move_file(
        &h,
        "Incoming/Alien (1979).mkv",
        "Alien (1979)/Alien (1979).mkv",
    );
    h.write("Alien (1979)/Alien (1979).nfo", &tmdb_movie(348));
    h.event("Alien (1979)/Alien (1979).mkv", FsEventKind::Created)
        .await;

    assert_eq!(h.file("Alien (1979)/Alien (1979).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:348".to_string())]);
    assert!(h.applied("Alien (1979)/Alien (1979).nfo").await.is_some());
}

/// An NFO a moved video finds beside its new path that its title never had
/// applied is a new file's NFO to that title: one naming another id than
/// the title's pin is kept against it, and the administrator told -- not
/// taken for an edit that replaces the pin.
#[tokio::test]
async fn a_video_moved_beside_a_conflicting_nfo_it_never_had_keeps_the_pin() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("One/Heat (1995).mkv");
    h.write("One/Heat (1995).nfo", &tmdb_movie(949));
    h.video("Incoming/Heat (1995).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    move_file(&h, "Incoming/Heat (1995).mkv", "Two/Heat (1995).mkv");
    h.write("Two/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Two/Heat (1995).nfo").await.is_some());
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("tmdb:1") && w.contains("tmdb:949")),
        "the administrator is told the pin was kept: {:?}",
        h.warnings().await
    );
}

/// A video moved alone, away from a folder NFO that stays where it was, did
/// not take that NFO with it: the conflicting NFO beside its new path is not
/// an edit of the one it left, and the title keeps its pin.
#[tokio::test]
async fn a_video_moved_away_from_an_nfo_that_stays_keeps_the_pin() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.video("One/Heat (1995).mkv");
    h.video("One/Heat (1995) - Remux.mkv");
    h.write("One/movie.nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    move_file(
        &h,
        "One/Heat (1995) - Remux.mkv",
        "Two/Heat (1995) - Remux.mkv",
    );
    h.write("Two/movie.nfo", &tmdb_movie(1));
    h.scan().await;

    assert!(h.applied("One/movie.nfo").await.is_some(), "still there");
    assert!(h.applied("Two/movie.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// A copy of Heat with its own NFO in each of `folders`, each indexed by a
/// scan of its own in turn: the first folder's NFO, naming tmdb:949, pins
/// the movie, and every later one -- naming tmdb:1, tmdb:2, ... -- is kept
/// against it and recorded as applied (FR-219). Each copy has its own size:
/// two same-size copies written within one filesystem timestamp tick would
/// stat alike, and a scan does not re-hash a file whose size and
/// modification time match its row.
async fn kept_conflicting_nfos_in(folders: &[&str]) -> Harness {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    for (i, folder) in folders.iter().enumerate() {
        let id = if i == 0 { 949 } else { i as u32 };
        h.write(&format!("{folder}/Heat (1995).mkv"), &"v".repeat(16 + i));
        h.write(&format!("{folder}/Heat (1995).nfo"), &tmdb_movie(id));
        h.scan().await;
    }
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    for folder in folders {
        assert!(
            h.applied(&format!("{folder}/Heat (1995).nfo"))
                .await
                .is_some()
        );
    }
    h
}

/// Rotate whole folders beneath the harness's root: the contents of
/// `folders[i]` end up in `folders[i + 1]`, and the last folder's in the
/// first. Two folders are swapped.
fn rotate_folders(h: &Harness, folders: &[&str]) {
    let parked = h.root.parent().unwrap().join("parked");
    let last = folders.len() - 1;
    std::fs::rename(h.root.join(folders[last]), &parked).unwrap();
    for i in (0..last).rev() {
        std::fs::rename(h.root.join(folders[i]), h.root.join(folders[i + 1])).unwrap();
    }
    std::fs::rename(&parked, h.root.join(folders[0])).unwrap();
}

/// Each folder's video row and the content hash of its NFO's record.
async fn rows_and_nfo_records(h: &Harness, folders: &[&str]) -> Vec<(Uuid, String)> {
    let mut found = Vec::new();
    for folder in folders {
        let id = h.file(&format!("{folder}/Heat (1995).mkv")).id;
        let record = h
            .applied(&format!("{folder}/Heat (1995).nfo"))
            .await
            .expect("the NFO is recorded");
        found.push((id, record.content_hash));
    }
    found
}

/// Folders swapped or rotated whole carry each kept NFO's applied state as
/// a move does (FR-219, issue #180): every row relinks with its NFO's
/// record, and the kept, conflicting NFOs stay kept -- through the scan
/// that finds the change and every scan after it -- whichever way the
/// folders' names sort.
#[tokio::test]
async fn folders_swapped_or_rotated_keep_the_pin_whichever_way_they_sort() {
    let cases: [&[&str]; 4] = [&["A", "B"], &["B", "A"], &["A", "B", "C"], &["C", "B", "A"]];
    for folders in cases {
        let h = kept_conflicting_nfos_in(folders).await;
        let before = rows_and_nfo_records(&h, folders).await;

        rotate_folders(&h, folders);
        h.scan().await;

        let after = rows_and_nfo_records(&h, folders).await;
        let mut expected = before.clone();
        expected.rotate_right(1);
        assert_eq!(
            after, expected,
            "each row and its NFO's record follow the folder, {folders:?}"
        );
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "{folders:?}"
        );
        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "{folders:?}"
        );
    }
}

/// Two folders swapped, one of whose NFOs was also edited on the way: that
/// NFO holds content no record does, so it is an edited NFO and replaces
/// the pin with the id it now names -- whether it is the pinning NFO or the
/// kept one -- while the other still carries its record.
#[tokio::test]
async fn folders_swapped_with_one_nfo_edited_repin_to_the_edit() {
    for edited in ["A", "B"] {
        let folders = ["A", "B"];
        let h = kept_conflicting_nfos_in(&folders).await;
        let kept_record = h.applied("B/Heat (1995).nfo").await.unwrap().content_hash;
        let pinning_record = h.applied("A/Heat (1995).nfo").await.unwrap().content_hash;

        rotate_folders(&h, &folders);
        h.write(&format!("{edited}/Heat (1995).nfo"), &tmdb_movie(2));
        h.scan().await;

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "edited in {edited}"
        );
        let carried = if edited == "A" {
            ("B/Heat (1995).nfo", pinning_record)
        } else {
            ("A/Heat (1995).nfo", kept_record)
        };
        assert_eq!(
            h.applied(carried.0).await.unwrap().content_hash,
            carried.1,
            "the other NFO carries its record, edited in {edited}"
        );
    }
}

/// Two folders swapped through a third name, as the watcher reports it: the
/// third name's removal, then each folder's directory event, in either
/// order. Each folder's video holds another row's content and is left to
/// the scan, and so is the NFO beside it: the watcher cannot yet tell a
/// moved NFO from an edited one. So the kept NFO stays kept -- whichever
/// folder held the pinning NFO, and whichever event comes first -- and the
/// scan then carries each NFO with its video (FR-219).
#[tokio::test]
async fn folders_swapped_under_the_watcher_leave_their_nfos_to_the_scan_and_keep_the_pin() {
    for folders in [["A", "B"], ["B", "A"]] {
        for events in [["A", "B"], ["B", "A"]] {
            let h = kept_conflicting_nfos_in(&folders).await;
            let before = rows_and_nfo_records(&h, &folders).await;

            std::fs::rename(h.root.join("A"), h.root.join("T")).unwrap();
            std::fs::rename(h.root.join("B"), h.root.join("A")).unwrap();
            std::fs::rename(h.root.join("T"), h.root.join("B")).unwrap();
            h.event("T", FsEventKind::Removed).await;
            for folder in events {
                h.event(folder, FsEventKind::Created).await;
            }

            assert_eq!(
                h.movie_pins(),
                vec![Some("tmdb:949".to_string())],
                "after the events, pinned by {folders:?}, events {events:?}"
            );
            assert_eq!(
                rows_and_nfo_records(&h, &folders).await,
                before,
                "rows and NFO records left for the scan, {folders:?}, {events:?}"
            );
            h.scan().await;
            let mut expected = before.clone();
            expected.reverse();
            assert_eq!(
                rows_and_nfo_records(&h, &folders).await,
                expected,
                "each row and its NFO's record follow the folder, {folders:?}, {events:?}"
            );
            assert_eq!(
                h.movie_pins(),
                vec![Some("tmdb:949".to_string())],
                "after the scan, pinned by {folders:?}, events {events:?}"
            );
        }
    }
}

/// Two folders swapped with one NFO edited on the way, as a watcher that
/// reports each file hears it, the NFOs first: the edited NFO holds content
/// no record does, but the video it describes is no longer the file its row
/// records, so it too is left to the scan rather than applied -- which would
/// overwrite the record that shows the other NFO moved. The scan then
/// carries the other NFO and replaces the pin with the edit, as a scan alone
/// does.
#[tokio::test]
async fn folders_swapped_with_one_nfo_edited_under_per_file_events_repin_to_the_edit() {
    for edited in ["A", "B"] {
        let folders = ["A", "B"];
        let other = if edited == "A" { "B" } else { "A" };
        let h = kept_conflicting_nfos_in(&folders).await;

        rotate_folders(&h, &folders);
        h.write(&format!("{edited}/Heat (1995).nfo"), &tmdb_movie(2));
        for rel in [
            format!("{edited}/Heat (1995).nfo"),
            format!("{other}/Heat (1995).nfo"),
            format!("{edited}/Heat (1995).mkv"),
            format!("{other}/Heat (1995).mkv"),
        ] {
            h.event(&rel, FsEventKind::Created).await;
        }
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "left for the scan, edited in {edited}"
        );

        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "edited in {edited}"
        );
    }
}

/// A video moved into a new folder while another took its old name is left
/// to the scan by the new folder's event, and so is the NFO beside it -- one
/// its title never had, describing no indexed file yet. Applied then, it
/// would be recorded having pinned nothing; left, the scan relinks the
/// video and applies the NFO as a new file's, pinning the title.
#[tokio::test]
async fn an_nfo_beside_a_video_left_to_the_scan_is_applied_by_the_scan() {
    let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
    h.write("One/Heat (1995).mkv", "one");
    h.write("Two/Heat (1995).mkv", "two, a longer copy");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None]);
    let id = h.file("Two/Heat (1995).mkv").id;

    move_file(&h, "Two/Heat (1995).mkv", "New/Heat (1995).mkv");
    move_file(&h, "One/Heat (1995).mkv", "Two/Heat (1995).mkv");
    h.write("New/Heat (1995).nfo", &tmdb_movie(5));
    h.event("New", FsEventKind::Created).await;

    assert!(
        h.applied("New/Heat (1995).nfo").await.is_none(),
        "left to the scan with its video"
    );
    h.scan().await;
    assert_eq!(h.file("New/Heat (1995).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:5".to_string())]);
}

/// A kept NFO moved with its video file by file, its own event heard before
/// the video's: it holds what its old path's record holds, and the file
/// there is gone, so the watcher does not take it for a new NFO and replace
/// the pin with it. The video's event then relinks the video and carries
/// the NFO's record with it.
#[tokio::test]
async fn a_kept_nfos_event_heard_before_its_moved_videos_keeps_the_pin() {
    let h = a_kept_conflicting_nfo_in_its_own_folder().await;
    let id = h.file("Two/Heat (1995).mkv").id;

    move_file(&h, "Two/Heat (1995).mkv", "Moved/Heat (1995).mkv");
    move_file(&h, "Two/Heat (1995).nfo", "Moved/Heat (1995).nfo");
    h.event("Moved/Heat (1995).nfo", FsEventKind::Created).await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(
        h.applied("Moved/Heat (1995).nfo").await.is_none(),
        "left for its video's relink"
    );

    h.event("Moved/Heat (1995).mkv", FsEventKind::Created).await;
    h.event("Two/Heat (1995).mkv", FsEventKind::Removed).await;
    h.event("Two/Heat (1995).nfo", FsEventKind::Removed).await;

    assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
    assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// Move each `(from, to)` video on disk, and return the batch a relink of
/// them hands [`LocalIndexService::carry_nfos_on_relink`]: each row's old
/// path, and the row at its new one.
fn move_videos(h: &Harness, moves: &[(&str, &str)]) -> Vec<(PathBuf, MediaFile)> {
    moves
        .iter()
        .map(|(from, to)| {
            let row = h.file(from);
            move_file(h, from, to);
            (
                h.root.join(from),
                MediaFile {
                    path: h.root.join(to),
                    ..row
                },
            )
        })
        .collect()
}

/// A `movie.nfo` two relinked videos share -- moved from one video's old
/// folder and edited on the way, beside another video that had no NFO -- is
/// judged over both of them, never for whichever the batch holds first
/// (FR-219): edited in the move in either order, so it is left for the
/// re-apply, which replaces the pin with it.
#[tokio::test]
async fn a_shared_movie_nfo_edited_in_a_move_is_judged_the_same_in_either_batch_order() {
    for reversed in [false, true] {
        let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
        h.video("P/Heat (1995).mkv");
        h.write("P/movie.nfo", &tmdb_movie(949));
        h.video("Q/Heat (1995) - Remux.mkv");
        h.scan().await;
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

        let mut batch = move_videos(
            &h,
            &[
                ("P/Heat (1995).mkv", "N/Heat (1995) - X.mkv"),
                ("Q/Heat (1995) - Remux.mkv", "N/Heat (1995) - Y.mkv"),
            ],
        );
        move_file(&h, "P/movie.nfo", "N/movie.nfo");
        h.write("N/movie.nfo", &tmdb_movie(2));
        if reversed {
            batch.reverse();
        }
        h.service
            .carry_nfos_on_relink(&h.library, &batch)
            .await
            .unwrap();

        assert!(
            h.applied("N/movie.nfo").await.is_none(),
            "edited in the move, reversed: {reversed}"
        );
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "reversed: {reversed}"
        );
    }
}

/// The same for a `tvshow.nfo` above two relinked episodes' season folder.
#[tokio::test]
async fn a_shared_tvshow_nfo_edited_in_a_move_is_judged_the_same_in_either_batch_order() {
    for reversed in [false, true] {
        let h = Harness::build(Probe::ContentHashed, Arc::new(RealClock)).await;
        h.video("P/Lost/Season 01/Lost - S01E01.mkv");
        h.write("P/Lost/tvshow.nfo", &tmdb_show(4607));
        h.video("Q/Lost/Season 01/Lost - S01E02.mkv");
        h.scan().await;
        assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);

        let mut batch = move_videos(
            &h,
            &[
                (
                    "P/Lost/Season 01/Lost - S01E01.mkv",
                    "N/Lost/Season 01/Lost - S01E01.mkv",
                ),
                (
                    "Q/Lost/Season 01/Lost - S01E02.mkv",
                    "N/Lost/Season 01/Lost - S01E02.mkv",
                ),
            ],
        );
        move_file(&h, "P/Lost/tvshow.nfo", "N/Lost/tvshow.nfo");
        h.write("N/Lost/tvshow.nfo", &tmdb_show(2));
        if reversed {
            batch.reverse();
        }
        h.service
            .carry_nfos_on_relink(&h.library, &batch)
            .await
            .unwrap();

        assert!(
            h.applied("N/Lost/tvshow.nfo").await.is_none(),
            "edited in the move, reversed: {reversed}"
        );
        assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
        h.scan().await;
        assert_eq!(
            h.show_pins(),
            vec![Some("tmdb:2".to_string())],
            "reversed: {reversed}"
        );
    }
}
