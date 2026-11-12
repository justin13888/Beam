//! A title's identity key, and retiring titles with no files left (issue
//! #183).
//!
//! The indexer used to find a movie or show by the same `title` column
//! enrichment overwrites, so a renamed title's next file created a duplicate;
//! and nothing ever removed a title whose files were all gone. These run the
//! real scan over a `TempDir` library with the movie and show doubles linked
//! to the file double, so liveness and orphan checks read the files the scan
//! actually wrote. The backfill of legacy titles and the rekey passes are in
//! the `backfill` and `rekey` modules, over this file's harness.

use super::*;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::access::AccessPolicy;
use beam_domain::models::{CreateLibrary, CreateMovie, CreateMovieEntry, Library, Movie, Show};
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use tempfile::TempDir;

/// What the service writes files through, built over the harness's in-memory
/// file repository.
type WrapFiles = fn(Arc<InMemoryFileRepository>) -> Arc<dyn FileRepository>;

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    show_repo: Arc<InMemoryShowRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    enrichment_repo: Arc<InMemoryEnrichmentStateRepository>,
    watch_state: Arc<InMemoryWatchStateRepository>,
    lists: Arc<beam_domain::repositories::user_list::InMemoryUserListRepository>,
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
        Self::build(grace, service_movies, None).await
    }

    /// [`Self::keeping_missing_files`], with the service writing files
    /// through `files`, built over the harness's in-memory file repository --
    /// to make a write fail part-way through a pass.
    async fn keeping_missing_files_with_files(files: WrapFiles) -> Self {
        Self::build(DEFAULT_MISSING_FILE_GRACE, None, Some(files)).await
    }

    async fn build(
        grace: Duration,
        service_movies: Option<Arc<dyn MovieRepository>>,
        service_files: Option<WrapFiles>,
    ) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::with_files(file_repo.clone()));
        let show_repo = Arc::new(InMemoryShowRepository::with_files(file_repo.clone()));
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let enrichment_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
        let watch_state = Arc::new(InMemoryWatchStateRepository::default());
        let lists =
            Arc::new(beam_domain::repositories::user_list::InMemoryUserListRepository::default());
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
        prober.expect_get_video_metadata().returning(|file| {
            Ok(VideoFileMetadata {
                file_path: file.path().to_path_buf(),
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
                chapters: Vec::new(),
            })
        });

        // The real clock: `delete_orphaned` compares its cutoff with the
        // `created_at` the repositories stamp from the wall clock.
        let service_files = match service_files {
            Some(wrap) => wrap(file_repo.clone()),
            None => file_repo.clone() as Arc<dyn FileRepository>,
        };
        let service = LocalIndexService::new(
            library_repo.clone(),
            service_files,
            service_movies.unwrap_or_else(|| movie_repo.clone() as Arc<dyn MovieRepository>),
            show_repo.clone(),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(hasher),
            Arc::new(prober),
            Arc::new(InMemoryNotificationService::new()),
            admin_log_repo.clone() as Arc<dyn AdminLogRepository>,
            watch_state.clone(),
        )
        .with_missing_file_grace(grace)
        .with_enrichment_repo(enrichment_repo.clone())
        .with_user_list_repo(lists.clone());

        Self {
            _dir: dir,
            root,
            library,
            library_repo,
            file_repo,
            movie_repo,
            show_repo,
            admin_log_repo,
            enrichment_repo,
            watch_state,
            lists,
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
        self.service.scan_library(self.library.id).await.unwrap();
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
            content_rating: None,
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
                })
                .await
                .unwrap();
            self.index_file(rel, MediaFileContent::movie(entry.id))
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
            content_rating: None,
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
        let (size_bytes, mtime) = read_fs_meta(path.parent().unwrap(), &path).unwrap();
        // Distinct from the scan's hashes, which count up from 1.
        let hash = u64::MAX - self.file_repo.files.lock().unwrap().len() as u64;
        self.file_repo
            .create(CreateMediaFile {
                library_id: self.library.id,
                path,
                hash,
                size_bytes,
                mtime,
                identity: None,
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

    /// The paths of `movie`'s default-edition files, grouped into sources as
    /// the sources listing stacks them (`stack_parts`, issue #233).
    async fn stacked_sources(&self, movie: Uuid) -> Vec<Vec<PathBuf>> {
        let entries = self
            .movie_repo
            .find_entries_by_movie_id(movie)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1, "one default edition: {entries:?}");
        let files: Vec<(PathBuf, Option<u32>)> = self
            .file_repo
            .find_by_movie_entry_id(entries[0].id)
            .await
            .unwrap()
            .into_iter()
            .map(|f| match f.content {
                Some(MediaFileContent::Movie { part_number, .. }) => (f.path, part_number),
                other => panic!("not a movie file: {other:?}"),
            })
            .collect();
        beam_domain::utils::source_rank::stack_parts(files, |(_, part)| part.map(|n| ((), n)))
            .into_iter()
            .map(|source| source.into_iter().map(|(path, _)| path).collect())
            .collect()
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
            access: Default::default(),
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

    let failed_walk = h.service.scan_library(h.library.id).await;
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

/// Keying titles indexed before identity keys existed.
#[path = "index_identity_backfill_tests.rs"]
mod backfill;

/// Re-deriving keys an older fold or inference derived (issue #182):
/// merges, what they carry to the kept title, and reclassification
/// waiting for the identity passes.
#[path = "index_identity_rekey_tests.rs"]
mod rekey;
