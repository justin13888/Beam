//! What a scan and the watcher make of the metadata a library keeps beside
//! its media (issue #184): Kodi NFOs, container tags and sidecar subtitles --
//! and that reading them never writes a byte into the library.
//!
//! These run the real scan over a `TempDir` library, with in-memory doubles
//! below the repository traits.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::probe::metadata::MetadataError;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::enrichment::{EnrichmentState, EnrichmentTargetId};
use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat};
use beam_domain::models::{CreateLibrary, Movie, Show};
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{AdminLogRepository, EnrichmentStateRepository};
use tempfile::TempDir;

/// Container tags the prober reports, by path.
type Tags = Arc<Mutex<HashMap<PathBuf, HashMap<String, String>>>>;

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    enrichment_repo: Arc<InMemoryEnrichmentStateRepository>,
    sidecar_repo: Arc<InMemorySidecarSubtitleRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    tags: Tags,
    /// While set, every probe fails, as one of a file still being muxed.
    probe_fails: Arc<AtomicBool>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::with_files(file_repo.clone()));
        let show_repo = Arc::new(InMemoryShowRepository::with_files(file_repo.clone()));
        let enrichment_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
        let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
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

        let service = LocalIndexService::new(
            library_repo,
            file_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(hasher),
            Arc::new(prober),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
        )
        .with_enrichment_repo(enrichment_repo.clone())
        .with_sidecar_repo(sidecar_repo.clone());

        Self {
            _dir: dir,
            root,
            library,
            file_repo,
            movie_repo,
            show_repo,
            enrichment_repo,
            sidecar_repo,
            admin_log_repo,
            tags,
            probe_fails,
            service,
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

    let pins: Vec<Option<String>> = h
        .movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .map(|m| m.pinned_ref.clone())
        .collect();
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

#[tokio::test]
async fn an_nfo_added_after_indexing_pins_the_movie_on_the_next_scan() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.scan().await;
    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);

    let nfo = h.write("Matrix/movie.nfo", MATRIX_NFO);
    std::fs::File::options()
        .write(true)
        .open(&nfo)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(3600))
        .unwrap();
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

#[tokio::test]
async fn a_scan_never_writes_into_the_library_root() {
    let h = Harness::new().await;
    furnished(&h);
    let before = snapshot(&h.root);

    h.scan().await;
    h.scan().await;
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.event("Matrix/matrix.1080p.en.srt", FsEventKind::Modified)
        .await;

    assert!(
        h.movie_of("Matrix/matrix.1080p.mkv").pinned_ref.is_some()
            && !h.subtitles_of("Matrix/matrix.1080p.mkv").await.is_empty(),
        "the scan did read what is beside the media"
    );
    assert_eq!(
        snapshot(&h.root),
        before,
        "the library tree is byte-for-byte unchanged"
    );
}

/// A read-only library -- a `:ro` mount, a share the server may not write --
/// indexes exactly as a writable one: nothing the scan does needs a write.
#[cfg(unix)]
#[tokio::test]
async fn a_read_only_library_is_indexed_in_full() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new().await;
    furnished(&h);
    let dirs: Vec<PathBuf> = walkdir::WalkDir::new(&h.root)
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.into_path())
        .collect();
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
    // Root ignores permissions; the premise of the test does not hold there.
    let root_user = std::fs::File::create(dirs[0].join("probe")).is_ok();
    if !root_user {
        h.scan().await;
    }
    set_mode(0o755);
    if root_user {
        return;
    }

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
