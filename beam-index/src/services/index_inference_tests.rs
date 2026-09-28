//! What a scan makes of real-world library layouts (issue #182): season
//! folders, specials, multi-episode files, editions, the files the path policy
//! keeps out, and reclassifying rows indexed under older rules.
//!
//! These run the real scan over a `TempDir` library, with the movie and show
//! doubles linked to the file double so orphan checks read the files the scan
//! actually wrote.

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::{CreateLibrary, CreateMovieEntry, Episode, Show};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use tempfile::TempDir;

/// What the prober reports for every file: an hour.
const PROBED_RUNTIME: Duration = Duration::from_secs(60 * 60);

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        Self::with_policy(PathPolicy::default()).await
    }

    /// A library whose missing files are purged at the first healthy scan,
    /// so a title left without files is retired in one scan.
    async fn with_policy(policy: PathPolicy) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::with_files(file_repo.clone()));
        let show_repo = Arc::new(InMemoryShowRepository::with_files(file_repo.clone()));
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let library = library_repo
            .create(CreateLibrary {
                name: "Inference".to_string(),
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
        let mut prober = MockMediaInfoService::new();
        prober.expect_get_video_metadata().returning(|path| {
            Ok(VideoFileMetadata {
                file_path: path.to_path_buf(),
                metadata: HashMap::default(),
                best_video_stream: None,
                best_audio_stream: None,
                best_subtitle_stream: None,
                duration: PROBED_RUNTIME.as_micros() as i64,
                streams: vec![],
                format_name: "matroska".to_string(),
                format_long_name: "Matroska".to_string(),
                file_size: 1024,
                bit_rate: 1000,
                probe_score: 100,
            })
        });

        // The real clock: `delete_orphaned` compares its cutoff with the
        // `created_at` the repositories stamp from the wall clock.
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
            Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        )
        .with_missing_file_grace(Duration::ZERO)
        .with_path_policy(policy);

        Self {
            _dir: dir,
            root,
            library,
            file_repo,
            movie_repo,
            show_repo,
            admin_log_repo,
            service,
        }
    }

    fn write(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
        path
    }

    async fn scan(&self) -> u32 {
        self.service
            .scan_library(self.library.id.to_string())
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

    fn present_files(&self) -> Vec<MediaFile> {
        self.file_repo
            .files
            .lock()
            .unwrap()
            .values()
            .filter(|f| f.missing_since.is_none())
            .cloned()
            .collect()
    }

    fn shows(&self) -> Vec<Show> {
        self.show_repo
            .shows
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    fn only_show(&self) -> Show {
        let shows = self.shows();
        assert_eq!(shows.len(), 1, "expected exactly one show: {shows:?}");
        shows.into_iter().next().unwrap()
    }

    /// The episode `rel`'s row is attached to, with its season number.
    fn episode_of(&self, rel: &str) -> (u32, Episode, Option<u32>) {
        let Some(MediaFileContent::Episode {
            episode_id,
            last_episode_number,
        }) = self.file(rel).content
        else {
            panic!("{rel} is not an episode file");
        };
        let episode = self.show_repo.episodes.lock().unwrap()[&episode_id].clone();
        let season = self.show_repo.seasons.lock().unwrap()[&episode.season_id].clone();
        (season.season_number, episode, last_episode_number)
    }

    fn show_of(&self, episode: &Episode) -> Uuid {
        self.show_repo.seasons.lock().unwrap()[&episode.season_id].show_id
    }

    /// Record `rel` as a build before issue #182 would have: classifier
    /// version 0, a probed runtime, `content` as given, and a size and mtime
    /// matching the disk so reconciling it touches no hasher or prober.
    async fn legacy_file(&self, rel: &str, content: Option<MediaFileContent>, runtime: Duration) {
        let path = self.write(rel);
        let (size_bytes, mtime) = read_fs_meta(&path).unwrap();
        let hash = u64::MAX - self.file_repo.files.lock().unwrap().len() as u64;
        self.file_repo
            .create(CreateMediaFile {
                library_id: self.library.id,
                path,
                hash,
                size_bytes,
                mtime,
                identity: None,
                mime_type: None,
                duration: Some(runtime),
                container_format: None,
                content: content.clone(),
                status: if content.is_some() {
                    FileStatus::Known
                } else {
                    FileStatus::Unknown
                },
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .unwrap();
    }

    async fn warnings(&self) -> Vec<String> {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|l| l.level == AdminLogLevel::Warning)
            .map(|l| l.message)
            .collect()
    }
}

// ─── shows ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_season_folder_names_its_series_and_the_episode_title_follows_the_marker() {
    let h = Harness::new().await;
    let rel = "Show Name/Season 01/Show.Name.S01E02.The.Title.1080p.mkv";
    h.write(rel);

    assert_eq!(h.scan().await, 1);

    let show = h.only_show();
    assert_eq!(show.title, "Show Name", "not the season folder's name");
    let (season, episode, last) = h.episode_of(rel);
    assert_eq!((season, episode.episode_number, last), (1, 2, None));
    assert_eq!(episode.title, "The Title");
    let file = h.file(rel);
    assert_eq!(file.status, FileStatus::Known);
    assert_eq!(file.classifier_version, CLASSIFIER_VERSION);
}

#[tokio::test]
async fn specials_are_season_zero_and_a_nameless_episode_gets_a_numbered_title() {
    let h = Harness::new().await;
    let rel = "Show Name/Specials/Show.Name.S00E01.mkv";
    h.write(rel);
    h.write("Show Name/Season 1/Show.Name.S01E01.mkv");

    h.scan().await;

    let (season, episode, _) = h.episode_of(rel);
    assert_eq!(season, 0);
    assert_eq!(episode.title, "Episode 1");
    assert_eq!(
        h.only_show().title,
        "Show Name",
        "both folders are one show"
    );
}

#[tokio::test]
async fn a_multi_episode_file_attaches_to_its_first_episode_and_carries_its_range() {
    let h = Harness::new().await;
    let rel = "Show/Season 01/Show.S01E01-E03.mkv";
    h.write(rel);

    h.scan().await;

    let (season, episode, last) = h.episode_of(rel);
    assert_eq!((season, episode.episode_number, last), (1, 1, Some(3)));
    assert_eq!(
        h.show_repo.episodes.lock().unwrap().len(),
        1,
        "the range is on the file, not extra episode rows"
    );
    // The probed hour is three episodes' runtime, not the first's (#189).
    assert_eq!(episode.runtime, None);
}

#[tokio::test]
async fn a_single_episode_file_gives_a_new_episode_its_probed_runtime() {
    let h = Harness::new().await;
    // A range of one is a single episode, however it is written.
    for rel in [
        "Show/Season 01/Show.S01E01.mkv",
        "Show/Season 01/Show.S01E02-E02.mkv",
    ] {
        h.write(rel);
    }

    h.scan().await;

    for rel in [
        "Show/Season 01/Show.S01E01.mkv",
        "Show/Season 01/Show.S01E02-E02.mkv",
    ] {
        let (_, episode, _) = h.episode_of(rel);
        assert_eq!(episode.runtime, Some(PROBED_RUNTIME), "{rel}");
    }
}

#[tokio::test]
async fn a_date_based_episode_is_filed_under_its_year_with_its_air_date() {
    let h = Harness::new().await;
    let rel = "The Daily Show/The.Daily.Show.2024.03.01.Guest.Name.720p.mkv";
    h.write(rel);

    h.scan().await;

    let (season, episode, _) = h.episode_of(rel);
    assert_eq!((season, episode.episode_number), (2024, 301));
    assert_eq!(episode.air_date.as_deref(), Some("2024-03-01"));
    assert_eq!(episode.title, "Guest Name");
}

#[tokio::test]
async fn a_season_folder_file_with_no_episode_number_is_kept_untitled_and_reported() {
    let h = Harness::new().await;
    let rel = "Show Name/Season 01/Behind the Scenes.mkv";
    h.write(rel);

    assert_eq!(h.scan().await, 1, "the file is still indexed");

    let file = h.file(rel);
    assert_eq!(file.status, FileStatus::Unknown);
    assert!(file.content.is_none(), "{:?}", file.content);
    assert!(h.shows().is_empty(), "no show is invented");
    assert!(
        h.movie_repo.movies.lock().unwrap().is_empty(),
        "and no movie either"
    );
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|m| m.contains("no episode number")),
        "the administrator is told"
    );
}

/// An unclassifiable file whose content later changes is re-probed and
/// refreshed, and stays `Unknown`: marking it `Known` with no movie or
/// episode is what the `files` CHECK refuses, and every later scan would
/// then fail on it.
#[tokio::test]
async fn a_changed_unclassifiable_file_is_refreshed_and_stays_unknown() {
    let h = Harness::new().await;
    let rel = "Show Name/Season 01/Behind the Scenes.mkv";
    let path = h.write(rel);
    h.scan().await;
    let before = h.file(rel);

    std::fs::write(&path, b"a re-encode, longer than what was there").unwrap();
    h.scan().await;

    let after = h.file(rel);
    assert_eq!(after.id, before.id);
    assert_ne!(after.hash, before.hash, "the new content was recorded");
    assert_ne!(after.size_bytes, before.size_bytes);
    assert_eq!(after.status, FileStatus::Unknown);
    assert!(after.content.is_none(), "{:?}", after.content);
}

/// Decision D182-C4: a fansub `<title> - <n>` whose folder spells the show
/// differently (romaji against English) is not a movie. It is kept untitled
/// and the administrator is told, rather than a season becoming dozens of
/// films.
#[tokio::test]
async fn an_absolute_number_no_folder_names_is_kept_untitled_and_reported() {
    let h = Harness::new().await;
    let rel = "Frieren (2023)/[SubsPlease] Sousou no Frieren - 12 (1080p).mkv";
    h.write(rel);

    assert_eq!(h.scan().await, 1, "the file is still indexed");

    let file = h.file(rel);
    assert_eq!(file.status, FileStatus::Unknown);
    assert!(file.content.is_none(), "{:?}", file.content);
    assert!(h.movie_repo.movies.lock().unwrap().is_empty(), "no movie");
    assert!(h.shows().is_empty(), "and no show");
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|m| m.contains("episode 12") && m.contains("no folder names the show")),
        "the administrator is told"
    );
}

/// A fractional `Show - 12.5` recap is kept untitled and reported, rather
/// than landing on episode 12 beside the real one.
#[tokio::test]
async fn a_fractional_episode_number_is_kept_untitled_and_reported() {
    let h = Harness::new().await;
    h.write("Show (1998)/[G] Show - 12 [1080p].mkv");
    let rel = "Show (1998)/[G] Show - 12.5 [1080p].mkv";
    h.write(rel);

    h.scan().await;

    let file = h.file(rel);
    assert_eq!(file.status, FileStatus::Unknown);
    assert!(file.content.is_none(), "{:?}", file.content);
    assert_eq!(h.shows().len(), 1, "episode 12 still makes its show");
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|m| m.contains("numbered 12.5") && m.contains(rel)),
        "the administrator is told"
    );
}

/// A file with no season and episode marker in a bare season-range folder
/// is kept untitled and reported, not indexed as a movie, while a marked
/// file beside it joins the series folder's show.
#[tokio::test]
async fn an_unmarked_file_in_a_season_range_folder_is_kept_untitled_and_reported() {
    let h = Harness::new().await;
    h.write("Breaking Bad (2008)/Season 1-2/Breaking.Bad.S02E01.mkv");
    let rel = "Breaking Bad (2008)/Season 1-2/Episode 1.mkv";
    h.write(rel);

    h.scan().await;

    let file = h.file(rel);
    assert_eq!(file.status, FileStatus::Unknown);
    assert!(file.content.is_none(), "{:?}", file.content);
    assert!(h.movie_repo.movies.lock().unwrap().is_empty(), "no movie");
    let shows = h.shows();
    assert_eq!(shows.len(), 1, "{shows:?}");
    assert_eq!(shows[0].title, "Breaking Bad");
    assert_eq!(shows[0].year, Some(2008), "the series folder's year");
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|m| m.contains("multi-season folder") && m.contains(rel)),
        "the administrator is told"
    );
}

// ─── movies ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn copies_of_one_edition_share_an_entry_and_another_edition_gets_its_own() {
    let h = Harness::new().await;
    h.write("Movie (2019)/Movie.2019.1080p.mkv");
    h.write("Movie (2019)/Movie.2019.2160p.mkv");
    h.write("Movie (2019)/Movie (2019) {edition-Director's Cut}.mkv");

    h.scan().await;

    assert_eq!(h.movie_repo.movies.lock().unwrap().len(), 1);
    let mut editions: Vec<Option<String>> = h
        .movie_repo
        .entries
        .lock()
        .unwrap()
        .values()
        .map(|e| e.edition.clone())
        .collect();
    editions.sort();
    assert_eq!(editions, vec![None, Some("Director's Cut".to_string())]);
    let entry = |rel: &str| match h.file(rel).content {
        Some(MediaFileContent::Movie { movie_entry_id }) => movie_entry_id,
        other => panic!("{rel} is not a movie file: {other:?}"),
    };
    assert_eq!(
        entry("Movie (2019)/Movie.2019.1080p.mkv"),
        entry("Movie (2019)/Movie.2019.2160p.mkv")
    );
}

// ─── the path policy ─────────────────────────────────────────────────────────

#[tokio::test]
async fn excluded_files_are_never_indexed() {
    let policy = PathPolicy::new(["Downloads"]).unwrap();
    let h = Harness::with_policy(policy).await;
    h.write("Movie (2019)/Movie.2019.mkv");
    for excluded in [
        "Movie (2019)/Extras/Making Of.mkv",
        // Extras of a yearless show, and a scene release's sample: neither
        // becomes a movie or a show of its own.
        "Breaking Bad/Extras/Making of Breaking Bad.mkv",
        "Breaking Bad/Featurettes/Inside Episode 1.mkv",
        "Breaking.Bad.S01E01.720p.HDTV.x264-GRP/Sample/sample-breaking.bad.s01e01.720p.hdtv.x264-grp.mkv",
        "Movie (2019)/Movie-trailer.mkv",
        "Movie (2019)/sample.mkv",
        "Movie (2019)/.Movie.2019.mkv",
        ".hidden/Other.2020.mkv",
        "@eaDir/Movie.2019.mkv/SYNOVIDEO.mkv",
        "Downloads/Partial.2021.mkv",
        "Movie (2019)/Movie.2019.en.srt",
        // Disc structures copied whole: no file in them is a film.
        "Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
        "Heat.1995.DVD9/VIDEO_TS/VTS_01_1.VOB",
        "Heat (1995)/BDMV/STREAM/00001.m2ts",
    ] {
        h.write(excluded);
    }

    assert_eq!(h.scan().await, 1);

    let paths: Vec<PathBuf> = h.present_files().into_iter().map(|f| f.path).collect();
    assert_eq!(paths, vec![h.root.join("Movie (2019)/Movie.2019.mkv")]);
}

#[tokio::test]
async fn a_legacy_sidecar_row_is_marked_missing() {
    let h = Harness::new().await;
    h.write("Movie (2019)/Movie.2019.mkv");
    // A build before issue #182 indexed sidecars as `Unknown` rows.
    let nfo = h.write("Movie (2019)/movie.nfo");
    let (size_bytes, mtime) = read_fs_meta(&nfo).unwrap();
    h.file_repo
        .create(CreateMediaFile {
            library_id: h.library.id,
            path: nfo.clone(),
            hash: 7,
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

    assert!(
        h.present_files().iter().all(|f| f.path != nfo),
        "the sidecar row no longer counts as a present file"
    );
}

/// The empty-root guard (issues #160, #197) counts video files the policy
/// excludes: a root holding only samples is mounted, and reconciling it is
/// right, where an empty mount point must still be refused.
#[tokio::test]
async fn a_root_of_only_samples_is_reconciled_not_refused() {
    let h = Harness::new().await;
    h.legacy_file(
        "Movie (2019)/Movie.2019.mkv",
        None,
        Duration::from_secs(90 * 60),
    )
    .await;
    std::fs::remove_file(h.root.join("Movie (2019)/Movie.2019.mkv")).unwrap();
    h.write("Movie (2019)/Movie.2019-sample.mkv");

    h.service
        .scan_library(h.library.id.to_string())
        .await
        .expect("a root with video files on it is not refused");

    assert!(h.present_files().is_empty());
}

#[tokio::test]
async fn a_watcher_event_for_an_excluded_file_indexes_nothing() {
    let h = Harness::new().await;
    let trailer = h.write("Movie (2019)/Trailers/Teaser.mkv");

    h.service
        .reconcile_path(h.library.id, trailer, FsEventKind::Created)
        .await
        .unwrap();

    assert!(h.file_repo.files.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_watcher_event_for_a_now_excluded_indexed_file_marks_it_missing() {
    let h = Harness::new().await;
    let rel = "Movie (2019)/movie.nfo";
    h.legacy_file(rel, None, Duration::from_secs(1)).await;

    h.service
        .reconcile_path(h.library.id, h.root.join(rel), FsEventKind::Modified)
        .await
        .unwrap();

    assert!(h.file(rel).missing_since.is_some());
}

// ─── reclassification ────────────────────────────────────────────────────────

/// Routed from the #183 review (C1): before issue #182 the show was named
/// after the file's parent folder, so a season-folder layout keyed a show
/// called "Season 01" (`season 01|`). The first scan under the new rules must
/// move those files onto the show their series folder names -- keeping each
/// row's id, and so its playback progress -- and retire the emptied husk.
#[tokio::test]
async fn reclassification_moves_a_season_folder_husk_onto_its_series() {
    let h = Harness::new().await;
    let legacy_runtime = Duration::from_secs(45 * 60);

    // The husk, as master's identity backfill keyed it.
    let husk = h
        .show_repo
        .find_or_create_by_identity(beam_domain::models::CreateShow::new("Season 01", None))
        .await
        .unwrap();
    assert_eq!(husk.identity_key.as_deref(), Some("season 01|"));
    let season = h.show_repo.find_or_create_season(husk.id, 1).await.unwrap();
    let rels = [
        "Show Name/Season 01/Show.Name.S01E01.mkv",
        "Show Name/Season 01/Show.Name.S01E02.mkv",
    ];
    for (n, rel) in rels.iter().enumerate() {
        let episode = h
            .show_repo
            .find_or_create_episode(beam_domain::models::CreateEpisode {
                season_id: season.id,
                episode_number: n as u32 + 1,
                title: "Show Name".to_string(),
                runtime: Some(legacy_runtime),
                air_date: None,
            })
            .await
            .unwrap();
        h.legacy_file(
            rel,
            Some(MediaFileContent::episode(episode.id)),
            legacy_runtime,
        )
        .await;
    }
    let ids_before: Vec<Uuid> = rels.iter().map(|rel| h.file(rel).id).collect();

    h.scan().await;

    let show = h.only_show();
    assert_ne!(show.id, husk.id, "the husk is retired");
    assert_eq!(show.title, "Show Name");
    assert_eq!(show.identity_key.as_deref(), Some("show name|"));
    for (rel, id_before) in rels.iter().zip(ids_before) {
        let file = h.file(rel);
        assert_eq!(file.id, id_before, "{rel} keeps its row");
        assert_eq!(file.classifier_version, CLASSIFIER_VERSION);
        assert_eq!(
            file.duration,
            Some(legacy_runtime),
            "{rel} was reclassified, not re-probed"
        );
        let (_, episode, _) = h.episode_of(rel);
        assert_eq!(h.show_of(&episode), show.id, "{rel} is on the series");
    }
    assert_eq!(
        h.show_repo.episodes.lock().unwrap().len(),
        2,
        "the husk's episodes went with it"
    );
}

#[tokio::test]
async fn a_row_already_on_the_current_rules_is_not_reclassified() {
    let h = Harness::new().await;
    let rel = "Movie (2019)/Movie.2019.mkv";
    h.write(rel);
    h.scan().await;
    let before = h.file(rel);

    // Point the row somewhere the path would not, then rescan: a current row
    // is trusted, so nothing moves it back.
    let entry = h
        .movie_repo
        .find_or_create_entry(CreateMovieEntry {
            library_id: h.library.id,
            movie_id: Uuid::new_v4(),
            edition: Some("Pinned".to_string()),
        })
        .await
        .unwrap();
    h.file_repo
        .set_classification(
            before.id,
            FileClassification {
                content: Some(MediaFileContent::Movie {
                    movie_entry_id: entry.id,
                }),
                status: FileStatus::Known,
                classifier_version: CLASSIFIER_VERSION,
            },
        )
        .await
        .unwrap();

    h.scan().await;

    assert!(matches!(
        h.file(rel).content,
        Some(MediaFileContent::Movie { movie_entry_id }) if movie_entry_id == entry.id
    ));
}

#[tokio::test]
async fn a_legacy_movie_that_now_reads_as_unclassifiable_loses_its_title() {
    let h = Harness::new().await;
    let rel = "Show Name/Season 01/Behind the Scenes.mkv";
    let legacy = h
        .movie_repo
        .find_or_create_by_identity(beam_domain::models::CreateMovie::new(
            "Behind the Scenes",
            None,
            None,
        ))
        .await
        .unwrap();
    let entry = h
        .movie_repo
        .find_or_create_entry(CreateMovieEntry {
            library_id: h.library.id,
            movie_id: legacy.id,
            edition: None,
        })
        .await
        .unwrap();
    h.legacy_file(
        rel,
        Some(MediaFileContent::Movie {
            movie_entry_id: entry.id,
        }),
        Duration::from_secs(600),
    )
    .await;

    h.scan().await;

    let file = h.file(rel);
    assert!(file.content.is_none());
    assert_eq!(file.status, FileStatus::Unknown);
    assert!(
        h.movie_repo.movies.lock().unwrap().is_empty(),
        "the invented movie is retired"
    );
}
