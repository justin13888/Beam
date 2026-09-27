//! A title's identity key, and retiring titles with no files left (issue
//! #183).
//!
//! The indexer used to find a movie or show by the same `title` column
//! enrichment overwrites, so a renamed title's next file created a duplicate;
//! and nothing ever removed a title whose files were all gone. These run the
//! real scan over a `TempDir` library with the movie and show doubles linked
//! to the file double, so liveness and orphan checks read the files the scan
//! actually wrote.

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::{
    CreateLibrary, CreateMovie, CreateMovieEntry, Library, Movie, MovieSearchQuery, Show,
    ShowSearchQuery,
};
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use tempfile::TempDir;

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    service: LocalIndexService,
}

impl Harness {
    /// A library whose files are purged the first healthy scan that misses
    /// them, so a test can retire a title in one scan.
    async fn purging_at_once() -> Self {
        Self::with_grace(Duration::ZERO).await
    }

    /// A library with the service's default grace: a missing file stays a
    /// soft-deleted row for the length of the test.
    async fn keeping_missing_files() -> Self {
        Self::with_grace(DEFAULT_MISSING_FILE_GRACE).await
    }

    async fn with_grace(grace: Duration) -> Self {
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
                name: "Identity".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();

        // Every file gets its own hash, so none reads as another's duplicate.
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
                duration: 60 * 60 * 1_000_000,
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
            library_repo.clone(),
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
        .with_missing_file_grace(grace);

        Self {
            _dir: dir,
            root,
            library,
            library_repo,
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

    fn remove(&self, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).unwrap();
    }

    async fn scan(&self) {
        self.service
            .scan_library(self.library.id.to_string())
            .await
            .unwrap();
    }

    fn movies(&self) -> Vec<Movie> {
        self.movie_repo
            .movies
            .lock()
            .unwrap()
            .values()
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

    fn only_movie(&self) -> Movie {
        let movies = self.movies();
        assert_eq!(movies.len(), 1, "expected exactly one movie: {movies:?}");
        movies.into_iter().next().unwrap()
    }

    fn only_show(&self) -> Show {
        let shows = self.shows();
        assert_eq!(shows.len(), 1, "expected exactly one show: {shows:?}");
        shows.into_iter().next().unwrap()
    }

    async fn completion_details(&self) -> serde_json::Value {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.message.contains("scan completed"))
            .and_then(|l| l.details)
            .expect("a completed scan logs its counts")
    }

    /// A movie as a build from before identity keys left it: no key, the
    /// display title enrichment wrote, and a present file at `rel` for each
    /// path given.
    async fn legacy_movie(&self, title: &str, year: Option<u32>, rels: &[&str]) -> Uuid {
        self.legacy_movie_created_at(title, year, rels, chrono::Utc::now())
            .await
    }

    /// [`Self::legacy_movie`], created at `created_at`.
    async fn legacy_movie_created_at(
        &self,
        title: &str,
        year: Option<u32>,
        rels: &[&str],
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        let now = created_at;
        let movie = Movie {
            id: Uuid::new_v4(),
            title: title.to_string(),
            identity_key: None,
            title_localized: None,
            description: None,
            year,
            release_date: None,
            runtime: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            rating_imdb: None,
            created_at: now,
            updated_at: now,
        };
        let id = movie.id;
        self.movie_repo.movies.lock().unwrap().insert(id, movie);
        for rel in rels {
            let entry = self
                .movie_repo
                .find_or_create_entry(CreateMovieEntry {
                    library_id: self.library.id,
                    movie_id: id,
                    edition: None,
                    is_primary: true,
                })
                .await
                .unwrap();
            self.index_file(
                rel,
                MediaFileContent::Movie {
                    movie_entry_id: entry.id,
                },
            )
            .await;
        }
        id
    }

    /// A show from before identity keys, with one episode file per path.
    async fn legacy_show(&self, title: &str, rels: &[&str]) -> Uuid {
        let now = chrono::Utc::now();
        let show = Show {
            id: Uuid::new_v4(),
            title: title.to_string(),
            identity_key: None,
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: now,
            updated_at: now,
        };
        let id = show.id;
        self.show_repo.shows.lock().unwrap().insert(id, show);
        let season = self.show_repo.find_or_create_season(id, 1).await.unwrap();
        for (n, rel) in rels.iter().enumerate() {
            let episode = self
                .show_repo
                .find_or_create_episode(beam_domain::models::CreateEpisode {
                    season_id: season.id,
                    episode_number: n as u32 + 1,
                    title: format!("Episode {}", n + 1),
                    runtime: None,
                    air_date: None,
                })
                .await
                .unwrap();
            self.index_file(rel, MediaFileContent::episode(episode.id))
                .await;
        }
        id
    }

    /// Put `rel` on disk and record it as an earlier scan would have, with a
    /// matching size and mtime so reconciling it touches no hasher.
    async fn index_file(&self, rel: &str, content: MediaFileContent) {
        let path = self.write(rel);
        let (size_bytes, mtime) = read_fs_meta(&path).unwrap();
        // Distinct from the scan's hashes, which count up from 1.
        let hash = u64::MAX - self.file_repo.files.lock().unwrap().len() as u64;
        self.file_repo
            .create(CreateMediaFile {
                library_id: self.library.id,
                path,
                hash,
                size_bytes,
                mtime,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: None,
                content: Some(content),
                status: FileStatus::Known,
                classifier_version: 0,
            })
            .await
            .unwrap();
    }

    /// Soft-delete every file row, as a scan that found them all gone would.
    async fn mark_every_file_missing(&self) {
        let ids: Vec<Uuid> = self
            .file_repo
            .files
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        self.file_repo
            .mark_missing(ids, chrono::Utc::now())
            .await
            .unwrap();
    }

    async fn backfill_warning(&self) -> Option<serde_json::Value> {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.level == AdminLogLevel::Warning && l.message.contains("identity keys"))
            .and_then(|l| l.details)
    }
}

// ─── identity survives enrichment ────────────────────────────────────────────

#[tokio::test]
async fn a_renamed_movie_takes_its_next_file_instead_of_duplicating() {
    let h = Harness::keeping_missing_files().await;
    h.write("The.Matrix.1999.1080p.BluRay.x264.mkv");
    h.scan().await;
    let movie = h.only_movie();

    // Enrichment replaces the display title with the provider's spelling.
    h.movie_repo
        .apply_enrichment(
            movie.id,
            &MovieEnrichment {
                title: "The Matrix: Provider Cut".to_string(),
                year: Some(1999),
                tmdb_id: Some(603),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    h.write("The Matrix (1999)/The Matrix (1999).mkv");
    h.scan().await;

    let after = h.only_movie();
    assert_eq!(
        after.id, movie.id,
        "the second file joins the renamed movie"
    );
    assert_eq!(after.title, "The Matrix: Provider Cut");
    assert_eq!(
        h.movie_repo
            .find_entries_by_movie_id(movie.id)
            .await
            .unwrap()
            .len(),
        1,
        "both copies are files of the one default-edition entry"
    );
}

#[tokio::test]
async fn remakes_of_one_title_stay_two_movies() {
    let h = Harness::keeping_missing_files().await;
    h.write("Dune.1984.720p.mkv");
    h.write("Dune.2021.2160p.mkv");
    h.scan().await;

    let mut years: Vec<Option<u32>> = h.movies().iter().map(|m| m.year).collect();
    years.sort();
    assert_eq!(years, vec![Some(1984), Some(2021)]);
}

#[tokio::test]
async fn a_renamed_show_takes_its_next_episode_instead_of_duplicating() {
    let h = Harness::keeping_missing_files().await;
    h.write("Shogun/Shogun.S01E01.1080p.mkv");
    h.scan().await;
    let show = h.only_show();

    h.show_repo
        .apply_enrichment(
            show.id,
            &ShowEnrichment {
                title: "Shōgun".to_string(),
                year: Some(2024),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    h.write("Shogun/Shogun.S01E02.1080p.mkv");
    h.scan().await;

    let after = h.only_show();
    assert_eq!(
        after.id, show.id,
        "the second episode joins the renamed show"
    );
    assert_eq!(after.title, "Shōgun");
}

// ─── titles without files ────────────────────────────────────────────────────

#[tokio::test]
async fn a_title_whose_only_file_is_missing_is_hidden_but_kept() {
    let h = Harness::keeping_missing_files().await;
    h.write("Arrival.2016.mkv");
    h.write("Severance/Severance.S01E01.mkv");
    h.scan().await;
    let movie = h.only_movie();
    let show = h.only_show();

    h.remove("Arrival.2016.mkv");
    h.remove("Severance/Severance.S01E01.mkv");
    h.write("Other.2020.mkv");
    h.scan().await;

    let listed_movies: Vec<Uuid> = h
        .movie_repo
        .search(&MovieSearchQuery::default())
        .await
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert!(
        !listed_movies.contains(&movie.id),
        "hidden from browse at once"
    );
    assert!(
        h.movie_repo.find_by_id(movie.id).await.unwrap().is_some(),
        "but kept: its file is only soft-deleted"
    );
    assert!(
        h.show_repo
            .search(&ShowSearchQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(h.show_repo.find_by_id(show.id).await.unwrap().is_some());
    assert_eq!(h.completion_details().await["titles_removed"], 0);
}

#[tokio::test]
async fn a_title_whose_files_are_all_purged_is_retired_with_them() {
    let h = Harness::purging_at_once().await;
    h.write("Arrival.2016.mkv");
    h.write("Kept.2019.mkv");
    h.write("Severance/Severance.S01E01.mkv");
    h.write("Severance/Severance.S01E02.mkv");
    h.scan().await;
    assert_eq!(h.movies().len(), 2);

    h.remove("Arrival.2016.mkv");
    h.remove("Severance/Severance.S01E01.mkv");
    h.remove("Severance/Severance.S01E02.mkv");
    h.scan().await;

    let titles: Vec<String> = h.movies().into_iter().map(|m| m.title).collect();
    assert_eq!(
        titles,
        vec!["Kept".to_string()],
        "only the movie with a file stays"
    );
    assert!(
        h.shows().is_empty(),
        "the show with no episode file left goes"
    );
    assert!(
        h.show_repo.seasons.lock().unwrap().is_empty()
            && h.show_repo.episodes.lock().unwrap().is_empty(),
        "and its seasons and episodes with it"
    );
    assert_eq!(h.completion_details().await["titles_removed"], 2);
}

#[cfg(unix)]
#[tokio::test]
async fn a_scan_whose_walk_failed_retires_no_title() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::purging_at_once().await;
    h.write("Present.2020.mkv");
    // A title left with no file by some earlier purge.
    let orphan = h
        .movie_repo
        .find_or_create_by_identity(CreateMovie::new("Orphan", None, None))
        .await
        .unwrap();
    let locked = h.root.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not bind, so the failed walk this
        // test is about cannot be produced.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let failed_walk = h.service.scan_library(h.library.id.to_string()).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    failed_walk.unwrap();
    assert!(
        h.movie_repo.find_by_id(orphan.id).await.unwrap().is_some(),
        "a walk that could not read the whole tree retires nothing"
    );

    h.scan().await;
    assert!(
        h.movie_repo.find_by_id(orphan.id).await.unwrap().is_none(),
        "the next healthy walk does"
    );
}

// ─── backfilling titles from before identity keys ────────────────────────────

#[tokio::test]
async fn a_legacy_title_is_keyed_from_its_files_not_its_enriched_title() {
    let h = Harness::keeping_missing_files().await;
    let movie = h
        .legacy_movie(
            "Amélie: Provider Title",
            Some(2001),
            &["Amelie.2001.1080p.mkv", "Amelie (2001)/Amelie (2001).mkv"],
        )
        .await;
    let show = h
        .legacy_show(
            "Severance: Provider Title",
            &["Severance/Severance.S01E01.mkv"],
        )
        .await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 2);
    let movie = h.movie_repo.find_by_id(movie).await.unwrap().unwrap();
    assert_eq!(
        movie.identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "the files' parse, not the stored display title"
    );
    let show = h.show_repo.find_by_id(show).await.unwrap().unwrap();
    assert_eq!(
        show.identity_key,
        Some(title_identity_key("Severance", None))
    );
    assert_eq!(h.backfill_warning().await, None, "nothing to warn about");
}

#[tokio::test]
async fn a_legacy_title_with_no_file_is_keyed_from_its_stored_title() {
    let h = Harness::keeping_missing_files().await;
    let movie = h.legacy_movie("Ghost", Some(1990), &[]).await;

    h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(
        h.movie_repo
            .find_by_id(movie)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Ghost", Some(1990)))
    );
}

#[tokio::test]
async fn a_legacy_title_whose_files_are_all_missing_is_keyed_from_those_files() {
    let h = Harness::keeping_missing_files().await;
    let movie = h
        .legacy_movie(
            "Amélie: Provider Title",
            Some(2001),
            &["Amelie.2001.1080p.mkv"],
        )
        .await;
    let show = h
        .legacy_show(
            "Severance: Provider Title",
            &["Severance/Severance.S01E01.mkv"],
        )
        .await;
    // Upgraded while the volume was away: every file is only soft-deleted.
    h.mark_every_file_missing().await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 2);
    assert_eq!(
        h.movie_repo
            .find_by_id(movie)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "a missing file still names its title; the enriched title does not"
    );
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Severance", None))
    );
}

#[tokio::test]
async fn of_legacy_duplicates_the_oldest_takes_the_key() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
    let qualities = ["480p", "576p", "720p", "1080p", "1440p", "2160p"];
    // Duplicates the old title lookup created, one per file, inserted newest
    // first so neither insertion order nor chance hands the original the key.
    let mut created = Vec::new();
    for (age, quality) in qualities.iter().enumerate().rev() {
        let id = h
            .legacy_movie_created_at(
                "Amelie",
                Some(2001),
                &[&format!("Amelie.2001.{quality}.mkv")],
                base + chrono::Duration::days(age as i64),
            )
            .await;
        created.push((age, id));
    }
    created.sort();
    let original = created[0].1;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 1);
    assert_eq!(
        h.movie_repo
            .find_by_id(original)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "the original, which enrichment and progress most likely hang off"
    );
    let later: Vec<Uuid> = created[1..].iter().map(|(_, id)| *id).collect();
    let warning = h.backfill_warning().await.expect("the admin is told");
    let mut reported: Vec<Uuid> =
        serde_json::from_value(warning["duplicate_movies"].clone()).unwrap();
    reported.sort();
    let mut expected = later;
    expected.sort();
    assert_eq!(reported, expected, "every later duplicate is named");
}

#[tokio::test]
async fn a_failed_backfill_does_not_hold_up_the_scan_and_is_retried() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let dir = TempDir::new().unwrap();
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let library = library_repo
        .create(CreateLibrary {
            name: "Backfill".to_string(),
            root_path: dir.path().to_path_buf(),
            description: None,
        })
        .await
        .unwrap();
    // The database refuses the backfill's first read, then recovers. The
    // strict counts are the point: the second scan retries, the third does
    // not backfill again.
    let mut movie_repo = MockMovieRepository::new();
    let mut attempts = mockall::Sequence::new();
    movie_repo
        .expect_find_unkeyed()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|| Err(DbErr::Custom("connection reset".to_string())));
    movie_repo
        .expect_find_unkeyed()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|| Ok(Vec::new()));
    movie_repo.expect_delete_orphaned().returning(|_| Ok(0));
    let service = LocalIndexService::new(
        library_repo.clone(),
        Arc::new(InMemoryFileRepository::default()),
        Arc::new(movie_repo),
        Arc::new(InMemoryShowRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(LocalAdminLogService::new(
            Arc::new(InMemoryAdminLogRepository::default()) as Arc<dyn AdminLogRepository>,
        )),
    );

    service.scan_all_libraries().await.unwrap();
    assert!(
        library_repo
            .find_by_id(library.id)
            .await
            .unwrap()
            .unwrap()
            .last_scan_finished_at
            .is_some(),
        "the scan ran despite the failed backfill"
    );
    assert!(
        !*service.identity_backfill_done.lock().await,
        "a failed backfill is not recorded as done"
    );

    service.scan_all_libraries().await.unwrap();
    assert!(*service.identity_backfill_done.lock().await, "retried");

    service.scan_all_libraries().await.unwrap();
}

#[tokio::test]
async fn legacy_titles_that_cannot_be_keyed_are_left_keyless_and_reported() {
    let h = Harness::keeping_missing_files().await;
    // Two films the old title lookup merged under one row.
    let merged = h
        .legacy_movie("Dune", None, &["Dune.1984.mkv", "Dune.2021.mkv"])
        .await;
    // The duplicate the old lookup created beside a title that is keyed now.
    let keyed = h
        .movie_repo
        .find_or_create_by_identity(CreateMovie::new("Amelie", Some(2001), None))
        .await
        .unwrap();
    let duplicate = h
        .legacy_movie("Amelie", Some(2001), &["Amelie.2001.720p.mkv"])
        .await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 0);
    for id in [merged, duplicate] {
        assert_eq!(
            h.movie_repo
                .find_by_id(id)
                .await
                .unwrap()
                .unwrap()
                .identity_key,
            None
        );
    }
    assert_eq!(
        h.movie_repo
            .find_by_id(keyed.id)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        keyed.identity_key,
        "the keyed title keeps its key"
    );
    let warning = h.backfill_warning().await.expect("the admin is told");
    assert_eq!(warning["ambiguous_movies"], serde_json::json!([merged]));
    assert_eq!(warning["duplicate_movies"], serde_json::json!([duplicate]));
}

#[tokio::test]
async fn scanning_every_library_backfills_first_so_a_legacy_title_takes_its_next_file() {
    let h = Harness::keeping_missing_files().await;
    let legacy = h
        .legacy_movie(
            "The Matrix: Provider Cut",
            Some(1999),
            &["The.Matrix.1999.1080p.mkv"],
        )
        .await;
    h.write("The.Matrix.1999.2160p.mkv");

    h.service.scan_all_libraries().await.unwrap();

    assert_eq!(
        h.only_movie().id,
        legacy,
        "no second movie for the new file"
    );
    assert_eq!(
        h.movie_repo
            .find_entries_by_movie_id(legacy)
            .await
            .unwrap()
            .len(),
        1,
        "the new copy joins the legacy entry"
    );
    // The library list the scan walked is the harness's one library.
    assert_eq!(h.library_repo.find_all().await.unwrap().len(), 1);
}
