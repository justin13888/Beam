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
use beam_domain::models::{CreateLibrary, CreateMovie, CreateMovieEntry, Library, Movie, Show};
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
        Self::with_grace_and_movies(grace, None).await
    }

    /// [`Self::purging_at_once`], with the service reading movies from
    /// `movies` instead of the harness's in-memory repository -- to make a
    /// movie read fail.
    async fn purging_at_once_with_movies(movies: Arc<dyn MovieRepository>) -> Self {
        Self::with_grace_and_movies(Duration::ZERO, Some(movies)).await
    }

    async fn with_grace_and_movies(
        grace: Duration,
        service_movies: Option<Arc<dyn MovieRepository>>,
    ) -> Self {
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
            service_movies.unwrap_or_else(|| movie_repo.clone() as Arc<dyn MovieRepository>),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(hasher),
            Arc::new(prober),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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

    /// The rules version the file row at `rel` was classified by.
    fn file_version(&self, rel: &str) -> u16 {
        let path = self.root.join(rel);
        self.file_repo
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .expect("the file is indexed")
            .classifier_version
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
            pinned_ref: None,
            pin_source: None,
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
        self.legacy_show_created_at(title, rels, chrono::Utc::now())
            .await
    }

    /// [`Self::legacy_show`], created at `created_at`.
    async fn legacy_show_created_at(
        &self,
        title: &str,
        rels: &[&str],
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        let now = created_at;
        let show = Show {
            id: Uuid::new_v4(),
            title: title.to_string(),
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
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
                container_tags: None,
            })
            .await
            .unwrap();
    }

    /// A movie a build between issues #183 and #182 keyed: `key` is what the
    /// fold of that build made of its files, and no rules version is on it.
    async fn keyed_movie(
        &self,
        title: &str,
        year: Option<u32>,
        key: &str,
        rels: &[&str],
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        let id = self
            .legacy_movie_created_at(title, year, rels, created_at)
            .await;
        assert!(
            self.movie_repo
                .rekey(id, Some(key.to_string()), 0)
                .await
                .unwrap()
        );
        id
    }

    /// The show counterpart of [`Self::keyed_movie`].
    async fn keyed_show(
        &self,
        title: &str,
        key: &str,
        rels: &[&str],
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        let id = self.legacy_show_created_at(title, rels, created_at).await;
        assert!(
            self.show_repo
                .rekey(id, Some(key.to_string()), 0)
                .await
                .unwrap()
        );
        id
    }

    /// The show's episodes by number in season 1, each with its files'
    /// paths relative to the root.
    fn season_one(&self, show: Uuid) -> Vec<(u32, Vec<String>)> {
        let seasons = self.show_repo.seasons.lock().unwrap().clone();
        let episodes = self.show_repo.episodes.lock().unwrap().clone();
        let files = self.file_repo.files.lock().unwrap().clone();
        let mut out: Vec<(u32, Vec<String>)> = episodes
            .values()
            .filter(|e| {
                seasons
                    .get(&e.season_id)
                    .is_some_and(|s| s.show_id == show && s.season_number == 1)
            })
            .map(|e| {
                let mut paths: Vec<String> = files
                    .values()
                    .filter(|f| {
                        matches!(f.content, Some(MediaFileContent::Episode { episode_id, .. }) if episode_id == e.id)
                    })
                    .map(|f| {
                        f.path
                            .strip_prefix(&self.root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                paths.sort();
                (e.episode_number, paths)
            })
            .collect();
        out.sort();
        out
    }

    async fn enrich_show(&self, id: Uuid, tmdb_id: u32) {
        self.show_repo
            .apply_enrichment(
                id,
                &ShowEnrichment {
                    title: "Provider Title".to_string(),
                    tmdb_id: Some(tmdb_id),
                    ..Default::default()
                },
                &beam_domain::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
    }

    async fn enrich_movie(&self, id: Uuid, year: u32, tmdb_id: u32) {
        self.movie_repo
            .apply_enrichment(
                id,
                &MovieEnrichment {
                    title: "Provider Title".to_string(),
                    year: Some(year),
                    tmdb_id: Some(tmdb_id),
                    ..Default::default()
                },
                &beam_domain::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
    }

    async fn admin_log_details(&self, needle: &str) -> Option<serde_json::Value> {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.message.contains(needle))
            .and_then(|l| l.details)
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
            &beam_domain::models::enrichment::FieldLocks::none(),
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
            &beam_domain::models::enrichment::FieldLocks::none(),
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

    // The catalogue reads the same doubles the indexer wrote through.
    let catalog = beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository::new(
        h.movie_repo.clone(),
        h.show_repo.clone(),
        Arc::new(beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default()),
    );
    let listed: Vec<Uuid> = beam_domain::repositories::CatalogRepository::browse(
        &catalog,
        &beam_domain::models::catalog::CatalogQuery {
            filters: Default::default(),
            sort: beam_domain::models::catalog::CatalogSort {
                field: beam_domain::models::catalog::CatalogSortField::Title,
                direction: beam_domain::models::catalog::SortDirection::Asc,
            },
            seek: beam_domain::models::catalog::Seek::Forward(None),
            limit: std::num::NonZeroU32::new(100).unwrap(),
        },
    )
    .await
    .unwrap()
    .iter()
    .map(|position| position.id)
    .collect();
    let other = h.movies().into_iter().find(|m| m.title == "Other").unwrap();
    assert_eq!(
        listed,
        vec![other.id],
        "both titles hidden from browse at once"
    );
    assert!(
        h.movie_repo.find_by_id(movie.id).await.unwrap().is_some(),
        "but kept: its file is only soft-deleted"
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
    let show = h.legacy_show("Severance", &[]).await;

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
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Severance", None)),
        "no file row names it a husk"
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
    movie_repo
        .expect_find_keyed_before_version()
        .times(1)
        .returning(|_| Ok(Vec::new()));
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
        Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
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
        !service.identity_passes_succeeded.load(Ordering::SeqCst),
        "a failed backfill is not recorded as done"
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert!(
        service.identity_passes_succeeded.load(Ordering::SeqCst),
        "retried"
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
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

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

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

/// Upgrading straight from a build before issue #183: a show named after a
/// season folder has no key yet. The backfill must leave it keyless -- its
/// files derive the real series' key, and a husk holding that key would
/// capture every file of the series when the scan reclassifies them -- so
/// the series gets its own show and the husk is retired. A husk a provider
/// has matched since -- its display title now the provider's -- is still one.
#[tokio::test]
async fn an_unkeyed_season_folder_husk_is_retired_not_given_its_series_key() {
    for enriched in [false, true] {
        an_unkeyed_husk_is_retired(enriched).await;
    }
}

async fn an_unkeyed_husk_is_retired(enriched: bool) {
    let h = Harness::purging_at_once().await;
    let husk = h
        .legacy_show(
            "Season 05",
            &[
                "Show X/Season 05/Show.X.S05E01.mkv",
                "Show X/Season 05/Show.X.S05E02.mkv",
            ],
        )
        .await;
    if enriched {
        h.enrich_show(husk, 999).await;
    }
    h.write("Show X/Season 01/Show.X.S01E01.mkv");

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "the husk is retired, not adopted");
    assert_eq!(show.title, "Show X");
    assert_eq!(show.identity_key.as_deref(), Some("show x|"));
    assert_eq!(show.tmdb_id, None, "the husk's match does not carry over");
    let files = h.file_repo.files.lock().unwrap().clone();
    assert_eq!(files.len(), 3);
    for file in files.values() {
        let Some(MediaFileContent::Episode { episode_id, .. }) = file.content else {
            panic!("{} is not an episode file", file.path.display());
        };
        let episode = h.show_repo.episodes.lock().unwrap()[&episode_id].clone();
        let season = h.show_repo.seasons.lock().unwrap()[&episode.season_id].clone();
        assert_eq!(season.show_id, show.id, "{}", file.path.display());
    }
    assert!(
        h.backfill_warning().await.is_none(),
        "a husk left keyless is not a title the administrator must sort out"
    );
}

/// A keyless show whose files a legacy title lookup gathered from a season
/// folder and from a series folder is not a husk: releasing it would drop a
/// title real files were attached to by name. Its files name two shows, so
/// it is left keyless and reported like any title whose files disagree.
#[tokio::test]
async fn a_show_only_some_of_whose_files_sit_in_a_season_folder_is_not_a_husk() {
    let h = Harness::keeping_missing_files().await;
    let show = h
        .legacy_show(
            "The Office",
            &[
                "Show X/Season 01/Show.X.S01E01.mkv",
                "The Office/The.Office.S01E01.mkv",
            ],
        )
        .await;
    h.enrich_show(show, 2316).await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.ambiguous_shows, vec![show], "not released as a husk");
    let warning = h.backfill_warning().await.expect("the admin is told");
    assert_eq!(warning["ambiguous_shows"], serde_json::json!([show]));
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        None
    );
}

// ─── keys an older fold or inference derived (issue #182) ──────────────────

/// A deployment of #183 keyed `Grey's Anatomy` as `grey s anatomy|`; the
/// current fold keys its files `greys anatomy|`. The first scan re-derives
/// the key in place, so the title keeps its id and enrichment and takes its
/// files, present and missing, instead of a second show being created
/// beside it.
#[tokio::test]
async fn a_key_from_before_the_apostrophe_fold_is_rederived_in_place() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[
                "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv",
                "Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv",
            ],
            base,
        )
        .await;
    h.enrich_show(show, 1416).await;
    let movie = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            base,
        )
        .await;
    h.enrich_movie(movie, 2001, 161).await;
    // The second episode went missing before the upgrade.
    h.remove("Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv");
    let missing: Vec<Uuid> = h
        .file_repo
        .files
        .lock()
        .unwrap()
        .values()
        .filter(|f| f.path.ends_with("Greys.Anatomy.S01E02.mkv"))
        .map(|f| f.id)
        .collect();
    h.file_repo
        .mark_missing(missing, chrono::Utc::now())
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show, not a new one");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(
        h.season_one(show),
        vec![
            (
                1,
                vec!["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv".to_string()]
            ),
            (
                2,
                vec!["Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv".to_string()]
            ),
        ],
        "the present and the missing episode both stay on it"
    );
    let after = h.only_movie();
    assert_eq!(after.id, movie);
    assert_eq!(after.tmdb_id, Some(161));
    assert_eq!(after.identity_key.as_deref(), Some("oceans eleven|2001"));
    assert!(
        h.show_repo
            .find_keyed_before_version(CLASSIFIER_VERSION)
            .await
            .unwrap()
            .is_empty()
            && h.movie_repo
                .find_keyed_before_version(CLASSIFIER_VERSION)
                .await
                .unwrap()
                .is_empty(),
        "every key now carries the current version"
    );
    let details = h
        .admin_log_details("identity keys of")
        .await
        .expect("the administrator is told");
    assert_eq!(details["rekeyed"], 2);

    // The next file of either spelling joins it.
    h.write("Greys.Anatomy.S02E01.mkv");
    h.scan().await;
    assert_eq!(h.only_show().id, show);
}

/// Under #183's fold a folder's `Grey's Anatomy` and a scene release's
/// `Greys.Anatomy` were two shows. Now they are one key, so the two merge:
/// the matched show survives, though it is the newer, and takes the other's
/// episodes -- one episode per number, however many files -- while the
/// other is retired.
#[tokio::test]
async fn titles_the_current_fold_reads_as_one_are_merged_into_the_matched_one() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let scene = h
        .keyed_show(
            "Greys Anatomy",
            "greys anatomy|",
            &["Greys.Anatomy.S01E01.720p.mkv", "Greys.Anatomy.S01E02.mkv"],
            base,
        )
        .await;
    let folder = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_show(folder, 1416).await;
    // Two movies neither of which a provider matched: the older survives.
    let older = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            base,
        )
        .await;
    let newer = h
        .keyed_movie(
            "Oceans Eleven",
            Some(2001),
            "oceans eleven|2001",
            &["Oceans.Eleven.2001.1080p.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_eq!(show.id, folder, "the show a provider matched is kept");
    assert_eq!(show.tmdb_id, Some(1416));
    assert_eq!(show.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(
        h.season_one(folder),
        vec![
            (
                1,
                vec![
                    "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv".to_string(),
                    "Greys.Anatomy.S01E01.720p.mkv".to_string(),
                ]
            ),
            (2, vec!["Greys.Anatomy.S01E02.mkv".to_string()]),
        ],
        "both copies of episode 1 are sources of one episode"
    );
    assert!(h.show_repo.find_by_id(scene).await.unwrap().is_none());

    let movie = h.only_movie();
    assert_eq!(
        movie.id, older,
        "of two unmatched titles, the older is kept"
    );
    assert_eq!(movie.identity_key.as_deref(), Some("oceans eleven|2001"));
    let entries = h.movie_repo.find_entries_by_movie_id(older).await.unwrap();
    assert_eq!(entries.len(), 1, "one default-edition entry");
    let on_entry = h
        .file_repo
        .files
        .lock()
        .unwrap()
        .values()
        .filter(|f| matches!(f.content, Some(MediaFileContent::Movie { movie_entry_id }) if movie_entry_id == entries[0].id))
        .count();
    assert_eq!(on_entry, 2, "with both copies");
    assert!(h.movie_repo.find_by_id(newer).await.unwrap().is_none());

    let details = h
        .admin_log_details("identity keys of")
        .await
        .expect("the administrator is told");
    assert_eq!(
        details["merged_shows"],
        serde_json::json!([{ "kept": folder, "retired": scene }])
    );
    assert_eq!(
        details["merged_movies"],
        serde_json::json!([{ "kept": older, "retired": newer }])
    );

    // Every key is current now, so a second pass finds nothing to do, and a
    // second scan leaves the merged titles as they are.
    let IdentityRekey {
        rekeyed,
        merged_movies,
        merged_shows,
        ambiguous_movies,
        ambiguous_shows,
    } = h.service.rekey_stale_titles().await.unwrap();
    assert_eq!(rekeyed, 0);
    assert!(merged_movies.is_empty() && merged_shows.is_empty());
    assert!(ambiguous_movies.is_empty() && ambiguous_shows.is_empty());
    h.scan().await;
    assert_eq!(h.only_show().id, folder);
    assert_eq!(h.only_movie().id, older);
    assert_eq!(h.season_one(folder).len(), 2);
}

/// If another writer takes the key between the merge releasing the loser and
/// keying the survivor, the merge stops there: no file is moved onto a
/// survivor left on its stale key.
#[tokio::test]
async fn a_merge_whose_survivor_loses_the_key_moves_no_file() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let h = Harness::keeping_missing_files().await;
    let stale = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            chrono::Utc::now() - chrono::Duration::days(2),
        )
        .await;
    let holder = h
        .keyed_movie(
            "Oceans Eleven",
            Some(2001),
            "oceans eleven|2001",
            &["Oceans.Eleven.2001.1080p.mkv"],
            chrono::Utc::now() - chrono::Duration::days(1),
        )
        .await;
    let stale_row = h.movie_repo.find_by_id(stale).await.unwrap().unwrap();
    let holder_row = h.movie_repo.find_by_id(holder).await.unwrap().unwrap();
    let stale_entries = h.movie_repo.find_entries_by_movie_id(stale).await.unwrap();
    let files_before = h.file_repo.files.lock().unwrap().clone();

    // The movie repository as the pass sees it when a concurrent writer
    // takes `oceans eleven|2001` after the loser is released.
    let mut movies = MockMovieRepository::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    let listed = stale_row.clone();
    movies
        .expect_find_keyed_before_version()
        .returning(move |_| Ok(vec![listed.clone()]));
    movies
        .expect_find_by_id()
        .withf(move |id| *id == stale)
        .returning(move |_| Ok(Some(stale_row.clone())));
    movies
        .expect_find_entries_by_movie_id()
        .withf(move |id| *id == stale)
        .times(1)
        .returning(move |_| Ok(stale_entries.clone()));
    movies
        .expect_find_entries_by_movie_id()
        .withf(move |id| *id != stale)
        .never();
    movies
        .expect_find_by_identity_key()
        .withf(|key| key == "oceans eleven|2001")
        .returning(move |_| Ok(Some(holder_row.clone())));
    movies
        .expect_rekey()
        .withf(|_, key, _| key.is_none())
        .times(1)
        .returning(|_, _, _| Ok(true));
    movies
        .expect_rekey()
        .withf(|_, key, _| key.is_some())
        .times(1)
        .returning(|_, _, _| Ok(false));
    movies.expect_find_or_create_entry().never();
    movies.expect_ensure_library_association().never();
    let service = LocalIndexService::new(
        h.library_repo.clone(),
        h.file_repo.clone(),
        Arc::new(movies),
        h.show_repo.clone(),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(LocalAdminLogService::new(
            h.admin_log_repo.clone() as Arc<dyn AdminLogRepository>
        )),
        Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
    );

    let report = service.rekey_stale_titles().await.unwrap();

    assert!(report.merged_movies.is_empty(), "{report:?}");
    let files_after = h.file_repo.files.lock().unwrap().clone();
    for (id, before) in &files_before {
        assert_eq!(
            format!("{:?}", files_after[id].content),
            format!("{:?}", before.content),
            "{}",
            before.path.display()
        );
    }
}

/// A title whose every file is away at upgrade is rekeyed from those
/// files, so it is still found when they come back.
#[tokio::test]
async fn a_title_whose_files_are_all_missing_is_rederived_from_them() {
    let h = Harness::keeping_missing_files().await;
    let rel = "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv";
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[rel],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    h.mark_every_file_missing().await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key
            .as_deref(),
        Some("greys anatomy|")
    );

    // Back again: it is still the same show.
    h.write(rel);
    h.scan().await;
    assert_eq!(h.only_show().id, show);
}

/// A husk a build between #183 and #182 keyed (`season 05|`) is released,
/// not handed its series' key: the series gets its own show and the husk is
/// retired, as for a keyless husk. It is known by its key, so a husk a
/// provider has matched since -- its display title now the provider's -- is
/// still released, and its match does not pass to the series.
#[tokio::test]
async fn a_keyed_season_folder_husk_is_released_not_rekeyed() {
    for enriched in [false, true] {
        a_keyed_husk_is_released(enriched).await;
    }
}

async fn a_keyed_husk_is_released(enriched: bool) {
    let h = Harness::keeping_missing_files().await;
    let husk = h
        .keyed_show(
            "Season 05",
            "season 05|",
            &["Show X/Season 05/Show.X.S05E01.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    if enriched {
        h.enrich_show(husk, 999).await;
    }

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "enriched: {enriched}");
    assert_eq!(show.title, "Show X");
    assert_eq!(show.identity_key.as_deref(), Some("show x|"));
    assert_eq!(show.tmdb_id, None, "the husk's match does not carry over");
}

/// A title whose files now name two titles keeps its old key -- there is no
/// one key to give it -- and the administrator is told.
#[tokio::test]
async fn a_title_whose_files_name_two_titles_keeps_its_key_and_is_reported() {
    let h = Harness::keeping_missing_files().await;
    let merged = h
        .keyed_movie(
            "Dune",
            None,
            "dune|",
            &["Dune.1984.mkv", "Dune.2021.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;

    h.service.rekey_stale_titles().await.unwrap();

    assert_eq!(
        h.movie_repo
            .find_by_id(merged)
            .await
            .unwrap()
            .unwrap()
            .identity_key
            .as_deref(),
        Some("dune|")
    );
    let warning = h
        .admin_log_details("older naming rules")
        .await
        .expect("the administrator is told");
    assert_eq!(warning["ambiguous_movies"], serde_json::json!([merged]));
}

// ─── reclassification waits for the identity passes (issue #182) ────────────

const GREYS: &str = "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv";

/// A show a build between #183 and #182 keyed `grey s anatomy|`, which a
/// provider has matched since. Its file derives `greys anatomy|` now.
async fn stale_enriched_greys(h: &Harness) -> Uuid {
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[GREYS],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    h.enrich_show(show, 1416).await;
    show
}

/// The stale show is untouched: reclassification was held.
fn assert_held(h: &Harness, show: Uuid) {
    let after = h.only_show();
    assert_eq!(after.id, show, "the stale show is not retired");
    assert_eq!(after.tmdb_id, Some(1416), "it keeps its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("grey s anatomy|"));
    assert_eq!(
        h.season_one(show),
        vec![(1, vec![GREYS.to_string()])],
        "it keeps its file"
    );
    assert_eq!(h.file_version(GREYS), 0, "left for a later scan");
}

/// While the identity passes fail, no entry point -- the scan of every
/// library, the administrator's scan of one, a watcher event --
/// reclassifies a file older rules classified: moving it would find no title
/// by its new key, create one, and retire the stale title with its
/// enrichment. The first pass to succeed rekeys the title in place, and the
/// scan then reclassifies the file onto it.
#[tokio::test]
async fn a_failed_rekey_pass_holds_reclassification_until_a_pass_succeeds() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let mut movies = MockMovieRepository::new();
    let mut attempts = mockall::Sequence::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    movies
        .expect_find_keyed_before_version()
        .times(3)
        .in_sequence(&mut attempts)
        .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
    movies
        .expect_find_keyed_before_version()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|_| Ok(Vec::new()));
    movies.expect_delete_orphaned().returning(|_| Ok(0));
    let h = Harness::purging_at_once_with_movies(Arc::new(movies)).await;
    let show = stale_enriched_greys(&h).await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_held(&h, show);
    h.scan().await;
    assert_held(&h, show);
    h.service
        .reconcile_path(h.library.id, h.root.join(GREYS), FsEventKind::Modified)
        .await
        .unwrap();
    assert_held(&h, show);
    let warning = h
        .admin_log_details("current naming rules")
        .await
        .expect("the administrator is told");
    assert_eq!(warning["pass"], "re-derivation");

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(h.season_one(show), vec![(1, vec![GREYS.to_string()])]);
    assert_eq!(
        h.file_version(GREYS),
        CLASSIFIER_VERSION,
        "now reclassified"
    );
}

/// A watcher event for a file with nothing to reclassify -- classified by
/// the current rules, or never probed -- does not ask for the identity
/// passes, so a failing pass is not retried, and reported again, on every
/// event. The strict count is the point: the scan's attempt is the only one.
#[tokio::test]
async fn a_watcher_event_with_nothing_to_reclassify_does_not_retry_the_passes() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let mut movies = MockMovieRepository::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    movies
        .expect_find_keyed_before_version()
        .times(1)
        .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
    movies.expect_delete_orphaned().returning(|_| Ok(0));
    let h = Harness::purging_at_once_with_movies(Arc::new(movies)).await;
    const CURRENT: &str = "Severance/Season 1/Severance.S01E01.mkv";
    h.write(CURRENT);
    const UNPROBED: &str = "Severance/Season 1/Severance.S01E02.mkv";
    let unprobed = h.write(UNPROBED);
    let (size_bytes, mtime) = read_fs_meta(&unprobed).unwrap();
    h.file_repo
        .create(CreateMediaFile {
            library_id: h.library.id,
            path: unprobed,
            hash: u64::MAX,
            size_bytes,
            mtime,
            mime_type: Some("video/x-matroska".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Unknown,
            classifier_version: 0,
            container_tags: None,
        })
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.file_version(CURRENT), CLASSIFIER_VERSION);
    for rel in [CURRENT, UNPROBED, CURRENT] {
        h.service
            .reconcile_path(h.library.id, h.root.join(rel), FsEventKind::Modified)
            .await
            .unwrap();
    }

    let warnings = h
        .admin_log_repo
        .list(100, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|l| l.message.contains("current naming rules"))
        .count();
    assert_eq!(warnings, 1, "only the scan's failed pass is reported");
}

/// The administrator's scan of one library, arriving before any scan of
/// every library, runs the identity passes itself before it reclassifies.
#[tokio::test]
async fn a_scan_of_one_library_rekeys_before_it_reclassifies() {
    let h = Harness::purging_at_once().await;
    let show = stale_enriched_greys(&h).await;

    h.scan().await;

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(h.season_one(show), vec![(1, vec![GREYS.to_string()])]);
    assert_eq!(h.file_version(GREYS), CLASSIFIER_VERSION);

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.only_show().id, show);
}
