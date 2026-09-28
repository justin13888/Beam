//! Shared behavioural contracts for the repository traits.
//!
//! Each macro here expands to a suite of `#[tokio::test]`s written purely
//! against a trait. The same suite is instantiated over the in-memory double
//! (hermetic, always run) and -- under the opt-in `pg-integration` feature in
//! `beam-index` -- over the SeaORM implementation against a real Postgres.
//!
//! This is what makes the doubles legitimate. A test that drives an
//! `InMemory*` repository and asserts on its own `HashMap` proves nothing about
//! production; the *same* assertions, run over both implementations, constrain
//! both at once and turn any fake/Postgres divergence into a failure rather
//! than silent drift. It is the one exception AGENTS.md grants to "never test
//! the double".
//!
//! Ordering is asserted by advancing an injected
//! [`crate::services::TestClock`], never by sleeping: every implementation
//! under contract takes its `updated_at` from the injected [`crate::services::Clock`].

/// Fixtures the shared contracts are written against. Gated behind
/// `test-utils`: only test code builds one.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fixture {
    use uuid::Uuid;

    use crate::repositories::PlaybackProgressRepository;
    use crate::services::TestClock;

    /// Everything the [`crate::playback_progress_repository_contract`] suite
    /// needs from a backing store.
    ///
    /// Identifiers are allocated by the fixture rather than invented by the
    /// contract because a real Postgres enforces the `user_id`/`file_id`
    /// foreign keys: the in-memory fixture can hand back a bare
    /// [`Uuid::new_v4`], while the Postgres fixture must insert the referenced
    /// rows first. The contract itself stays identical across both.
    #[async_trait::async_trait]
    pub trait PlaybackProgressFixture: Send + Sync {
        /// The repository under contract, freshly empty of rows for the
        /// identifiers this fixture will hand out.
        fn repo(&self) -> &dyn PlaybackProgressRepository;

        /// The clock the repository stamps `updated_at` from.
        fn clock(&self) -> &TestClock;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;

        /// A media file that exists as far as the backing store is concerned.
        async fn new_file(&self) -> Uuid;

        /// Stamp `missing_since` on a file from [`Self::new_file`], as a scan
        /// that no longer finds it would (issue #179).
        async fn mark_file_missing(&self, file_id: Uuid);
    }

    /// Everything the [`crate::file_repository_contract`] suite needs from a
    /// backing store.
    ///
    /// The parents a file row hangs off are allocated by the fixture for the
    /// same reason as in [`PlaybackProgressFixture`]: Postgres enforces the
    /// `library_id`, `movie_entry_id` and `episode_id` foreign keys, the
    /// in-memory store does not.
    #[async_trait::async_trait]
    pub trait FileRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A movie entry inside `library_id`.
        async fn new_movie_entry(&self, library_id: Uuid) -> Uuid;

        /// An episode (with the show and season it needs) for `library_id`.
        async fn new_episode(&self, library_id: Uuid) -> Uuid;
    }

    /// Everything the [`crate::show_repository_contract`] suite needs from a
    /// backing store. Every show, season and episode the contract needs is
    /// created through the trait itself, so a real Postgres sees the same
    /// foreign-key chain the indexer builds.
    ///
    /// Whether a show is live, or orphaned, is a question about its files, so
    /// the fixture also hands out the file repository over the same store --
    /// linked to `repo` so the one sees the other's rows -- and a library for
    /// those files to belong to.
    ///
    /// `delete_orphaned` is global: over a database other tests share it
    /// would delete their fileless shows mid-test. A Postgres fixture must
    /// therefore give each test a store of its own.
    #[async_trait::async_trait]
    pub trait ShowRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::ShowRepository;

        /// The file repository over the same store.
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A show titled `title`, created at `created_at`, with no identity
        /// key -- a row from before keys existed, which the trait itself can
        /// no longer create.
        async fn new_unkeyed_show(
            &self,
            title: &str,
            created_at: ::chrono::DateTime<::chrono::Utc>,
        ) -> Uuid;
    }

    /// Everything the [`crate::movie_repository_contract`] suite needs from a
    /// backing store; the movie counterpart of [`ShowRepositoryFixture`], with
    /// the same requirement that each Postgres test own its store.
    #[async_trait::async_trait]
    pub trait MovieRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::MovieRepository;

        /// The file repository over the same store.
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A movie titled `title`, created at `created_at`, with no identity
        /// key.
        async fn new_unkeyed_movie(
            &self,
            title: &str,
            created_at: ::chrono::DateTime<::chrono::Utc>,
        ) -> Uuid;
    }

    /// Everything the [`crate::library_shape_repository_contract`] suite needs
    /// from a backing store: the aggregate under contract, and the repositories
    /// the contract seeds rows through -- all over one store, so what one
    /// writes the aggregate sees.
    ///
    /// The shape is global, so as for shows and movies a Postgres fixture must
    /// give each test a store of its own.
    pub trait LibraryShapeFixture: Send + Sync {
        /// The aggregate under contract.
        fn repo(&self) -> &dyn crate::repositories::LibraryShapeRepository;
        fn libraries(&self) -> &dyn crate::repositories::LibraryRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;
        fn files(&self) -> &dyn crate::repositories::FileRepository;
        fn streams(&self) -> &dyn crate::repositories::MediaStreamRepository;
    }

    /// Everything the [`crate::catalog_repository_contract`] suite needs from a
    /// backing store: the read model under contract, and the repositories the
    /// contract builds its titles through -- all over one store, so what one
    /// writes the catalogue lists.
    ///
    /// The catalogue is global, so as for shows and movies a Postgres fixture
    /// must give each test a store of its own.
    #[async_trait::async_trait]
    pub trait CatalogRepositoryFixture: Send + Sync {
        /// The read model under contract.
        fn repo(&self) -> &dyn crate::repositories::CatalogRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;
        fn genres(&self) -> &dyn crate::repositories::GenreRepository;
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::genre_repository_contract`] suite needs: the
    /// repository under contract and the title repositories over the same
    /// store, since a real Postgres enforces the junction tables' foreign keys.
    pub trait GenreRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::GenreRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;
    }

    /// Everything the [`crate::playback_telemetry_repository_contract`] suite
    /// needs from a backing store. The counters reference nothing, so the
    /// repository is all there is -- but `summarize` and `prune_before` are
    /// global, so a Postgres fixture must give each test a store of its own.
    pub trait PlaybackTelemetryFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::PlaybackTelemetryRepository;
    }
}

/// Behavioural contract for [`crate::repositories::PlaybackProgressRepository`].
///
/// `$setup` names an `async fn() -> impl PlaybackProgressFixture`.
#[macro_export]
macro_rules! playback_progress_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::models::playback_progress::UpsertPlaybackProgress;
        use $crate::repositories::contract::fixture::PlaybackProgressFixture as _;

        /// One progress report. `duration_secs` is `Some(100.0)` so `completed`
        /// is a function of `position_secs` alone -- the 95% threshold puts the
        /// boundary at 95.0.
        fn report(user_id: Uuid, file_id: Uuid, position_secs: f64) -> UpsertPlaybackProgress {
            UpsertPlaybackProgress {
                user_id,
                file_id,
                position_secs,
                duration_secs: Some(100.0),
            }
        }

        #[tokio::test]
        async fn upsert_updates_the_existing_row_rather_than_inserting_a_second() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let file = fixture.new_file().await;

            let first = repo.upsert(report(user, file, 10.0)).await.unwrap();
            let second = repo.upsert(report(user, file, 20.0)).await.unwrap();

            assert_eq!(first.id, second.id, "the same (user, file) row is reused");
            assert_eq!(second.position_secs, 20.0);
            assert_eq!(
                repo.count_by_user(user).await.unwrap(),
                1,
                "no duplicate row was inserted"
            );
        }

        #[tokio::test]
        async fn upsert_keeps_a_separate_row_per_user_and_per_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user_a = fixture.new_user().await;
            let user_b = fixture.new_user().await;
            let file = fixture.new_file().await;

            repo.upsert(report(user_a, file, 10.0)).await.unwrap();
            repo.upsert(report(user_b, file, 30.0)).await.unwrap();
            repo.upsert(report(user_a, fixture.new_file().await, 40.0))
                .await
                .unwrap();

            assert_eq!(repo.count_by_user(user_a).await.unwrap(), 2);
            assert_eq!(repo.count_by_user(user_b).await.unwrap(), 1);
            assert_eq!(
                repo.find_by_user_and_file(user_b, file)
                    .await
                    .unwrap()
                    .expect("user_b has a row for this file")
                    .position_secs,
                30.0,
                "one user's report must not overwrite another's for the same file"
            );
        }

        #[tokio::test]
        async fn upsert_derives_completed_from_position_and_reverses_it_on_rewind() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let file = fixture.new_file().await;

            let below = repo.upsert(report(user, file, 94.9)).await.unwrap();
            assert!(!below.completed, "94.9% is below the 95% threshold");

            let at = repo.upsert(report(user, file, 95.0)).await.unwrap();
            assert!(at.completed, "the threshold itself counts as completed");

            let rewound = repo.upsert(report(user, file, 5.0)).await.unwrap();
            assert!(
                !rewound.completed,
                "rewinding a finished item puts it back in progress"
            );
        }

        #[tokio::test]
        async fn upsert_stamps_updated_at_from_the_injected_clock() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let file = fixture.new_file().await;

            let first = repo.upsert(report(user, file, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(3600));
            let second = repo.upsert(report(user, file, 20.0)).await.unwrap();

            assert_eq!(
                (second.updated_at - first.updated_at).num_seconds(),
                3600,
                "updated_at advances with the clock, not with wall time"
            );
        }

        #[tokio::test]
        async fn find_by_user_and_file_is_none_until_a_report_arrives() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let file = fixture.new_file().await;

            assert!(
                repo.find_by_user_and_file(user, file)
                    .await
                    .unwrap()
                    .is_none()
            );

            let inserted = repo.upsert(report(user, file, 10.0)).await.unwrap();
            let found = repo
                .find_by_user_and_file(user, file)
                .await
                .unwrap()
                .expect("the row just written is readable");

            assert_eq!(found.id, inserted.id);
            assert_eq!(found.position_secs, 10.0);
            assert_eq!(found.duration_secs, Some(100.0));
        }

        #[tokio::test]
        async fn find_in_progress_excludes_completed_rows_and_other_users() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let other = fixture.new_user().await;
            let watching = fixture.new_file().await;

            repo.upsert(report(user, watching, 10.0)).await.unwrap();
            repo.upsert(report(user, fixture.new_file().await, 99.0))
                .await
                .unwrap();
            repo.upsert(report(other, fixture.new_file().await, 10.0))
                .await
                .unwrap();

            let in_progress = repo.find_in_progress_by_user(user, 10).await.unwrap();

            assert_eq!(in_progress.len(), 1);
            assert_eq!(in_progress[0].file_id, watching);
        }

        #[tokio::test]
        async fn find_in_progress_orders_most_recently_updated_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let oldest = fixture.new_file().await;
            let middle = fixture.new_file().await;
            let newest = fixture.new_file().await;

            repo.upsert(report(user, oldest, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            repo.upsert(report(user, middle, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            repo.upsert(report(user, newest, 10.0)).await.unwrap();

            let order: Vec<Uuid> = repo
                .find_in_progress_by_user(user, 10)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.file_id)
                .collect();

            assert_eq!(order, vec![newest, middle, oldest]);
        }

        #[tokio::test]
        async fn find_in_progress_limit_keeps_the_most_recent_rows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let oldest = fixture.new_file().await;
            let middle = fixture.new_file().await;
            let newest = fixture.new_file().await;

            repo.upsert(report(user, oldest, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            repo.upsert(report(user, middle, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            repo.upsert(report(user, newest, 10.0)).await.unwrap();

            let limited: Vec<Uuid> = repo
                .find_in_progress_by_user(user, 2)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.file_id)
                .collect();

            assert_eq!(
                limited,
                vec![newest, middle],
                "the limit truncates the tail of the ordering, not an arbitrary subset"
            );
        }

        #[tokio::test]
        async fn find_in_progress_with_a_zero_limit_returns_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            repo.upsert(report(user, fixture.new_file().await, 10.0))
                .await
                .unwrap();

            assert!(
                repo.find_in_progress_by_user(user, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn find_page_includes_completed_rows_most_recent_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let watched = fixture.new_file().await;
            let finished = fixture.new_file().await;

            repo.upsert(report(user, watched, 10.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            repo.upsert(report(user, finished, 99.0)).await.unwrap();

            let page = repo.find_page_by_user(user, 50, 0).await.unwrap();

            assert_eq!(page.len(), 2, "history includes completed rows");
            assert_eq!(page[0].file_id, finished);
            assert!(page[0].completed);
            assert_eq!(page[1].file_id, watched);
        }

        #[tokio::test]
        async fn find_page_slices_the_ordering_by_offset_and_limit() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let mut files = Vec::new();
            for _ in 0..5 {
                let file = fixture.new_file().await;
                repo.upsert(report(user, file, 10.0)).await.unwrap();
                clock.advance(Duration::from_secs(60));
                files.push(file);
            }
            // Newest first: files[4], files[3], files[2], files[1], files[0].
            let expected = vec![files[2], files[1]];

            let page: Vec<Uuid> = repo
                .find_page_by_user(user, 2, 2)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.file_id)
                .collect();

            assert_eq!(page, expected, "offset skips within the same ordering");
        }

        #[tokio::test]
        async fn find_page_past_the_end_is_empty_rather_than_wrapping() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            repo.upsert(report(user, fixture.new_file().await, 10.0))
                .await
                .unwrap();

            assert!(
                repo.find_page_by_user(user, 10, 5)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn count_by_user_counts_finished_and_in_progress_for_that_user_only() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let other = fixture.new_user().await;

            assert_eq!(repo.count_by_user(user).await.unwrap(), 0);

            repo.upsert(report(user, fixture.new_file().await, 10.0))
                .await
                .unwrap();
            repo.upsert(report(user, fixture.new_file().await, 99.0))
                .await
                .unwrap();
            repo.upsert(report(other, fixture.new_file().await, 10.0))
                .await
                .unwrap();

            assert_eq!(repo.count_by_user(user).await.unwrap(), 2);
            assert_eq!(repo.count_by_user(other).await.unwrap(), 1);
        }

        #[tokio::test]
        async fn find_in_progress_drops_missing_files_before_the_limit_applies() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let visible = fixture.new_file().await;
            repo.upsert(report(user, visible, 10.0)).await.unwrap();
            // More missing rows than the limit, every one newer than the
            // visible row: filtered after the limit, they would fill it.
            for _ in 0..3 {
                clock.advance(Duration::from_secs(60));
                let missing = fixture.new_file().await;
                repo.upsert(report(user, missing, 10.0)).await.unwrap();
                fixture.mark_file_missing(missing).await;
            }

            let rows: Vec<Uuid> = repo
                .find_in_progress_by_user(user, 2)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.file_id)
                .collect();

            assert_eq!(rows, vec![visible]);
        }

        /// A file's last play is its latest progress row's, across users;
        /// a missing file keeps its own, and a file never played, or not
        /// asked after, has none.
        #[tokio::test]
        async fn last_played_at_is_the_latest_report_for_each_file_asked() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (alice, bob) = (fixture.new_user().await, fixture.new_user().await);
            let watched = fixture.new_file().await;
            let gone = fixture.new_file().await;
            let unplayed = fixture.new_file().await;
            let unasked = fixture.new_file().await;

            let first = repo.upsert(report(alice, watched, 10.0)).await.unwrap();
            fixture.clock().advance(Duration::from_secs(60));
            let latest = repo.upsert(report(bob, watched, 20.0)).await.unwrap();
            let missing = repo.upsert(report(alice, gone, 30.0)).await.unwrap();
            repo.upsert(report(alice, unasked, 40.0)).await.unwrap();
            fixture.mark_file_missing(gone).await;

            let last = repo
                .last_played_at(vec![watched, gone, unplayed])
                .await
                .unwrap();

            assert!(latest.updated_at > first.updated_at);
            assert_eq!(last.get(&watched), Some(&latest.updated_at));
            assert_eq!(last.get(&gone), Some(&missing.updated_at));
            assert_eq!(last.len(), 2, "{last:?}");
            assert!(repo.last_played_at(Vec::new()).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn history_pages_and_counts_only_rows_whose_file_is_present() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let visible = fixture.new_file().await;
            repo.upsert(report(user, visible, 99.0)).await.unwrap();
            clock.advance(Duration::from_secs(60));
            let missing = fixture.new_file().await;
            repo.upsert(report(user, missing, 10.0)).await.unwrap();
            fixture.mark_file_missing(missing).await;

            let page: Vec<Uuid> = repo
                .find_page_by_user(user, 1, 0)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.file_id)
                .collect();

            assert_eq!(page, vec![visible], "the first page is not a missing row");
            assert_eq!(
                repo.count_by_user(user).await.unwrap(),
                1,
                "the total counts the rows the pages hold"
            );
            assert!(
                repo.find_by_user_and_file(user, missing)
                    .await
                    .unwrap()
                    .is_some(),
                "the missing file's progress is kept, only hidden"
            );
        }
    };
}

/// Behavioural contract for [`crate::repositories::FileRepository`]: the
/// soft-delete lifecycle of issue #179 -- which reads hide a missing file,
/// which see it, and that marking, restoring and purging keep the id and the
/// first stamp.
///
/// `$setup` names an `async fn() -> impl FileRepositoryFixture`.
#[macro_export]
macro_rules! file_repository_contract {
    ($setup:path) => {
        use ::chrono::{DateTime, Utc};
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::file::{
            CreateMediaFile, FileClassification, FileRelink, FileStatus, MediaFile,
            MediaFileContent, ProbeUpdate, UpdateMediaFile, displaced_path,
        };
        use $crate::repositories::contract::fixture::FileRepositoryFixture;

        /// A fixed, non-epoch instant: `missing_since` is `timestamptz`, and a
        /// whole second survives Postgres's microsecond precision unchanged.
        fn at(offset_secs: i64) -> DateTime<Utc> {
            DateTime::from_timestamp(1_700_000_000 + offset_secs, 0).expect("valid instant")
        }

        /// Insert a present file under `library_id`. The path and hash are
        /// fresh per call so concurrently running Postgres tests never meet.
        async fn file_in(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            content: MediaFileContent,
        ) -> MediaFile {
            let unique = Uuid::new_v4();
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path: PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
                    // Positive and unique: the hash is a signed BIGINT column.
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .expect("create a file")
        }

        async fn movie_file(fixture: &impl FileRepositoryFixture, library_id: Uuid) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            file_in(
                fixture,
                library_id,
                MediaFileContent::Movie { movie_entry_id },
            )
            .await
        }

        fn ids(files: &[MediaFile]) -> Vec<Uuid> {
            let mut ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
            ids.sort();
            ids
        }

        fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
            ids.sort();
            ids
        }

        /// One row per path (issue #181): a second file at a path is refused
        /// whatever its hash, and the first row is left as it was.
        #[tokio::test]
        async fn a_second_file_at_one_path_is_refused_whatever_its_hash() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let first = movie_file(&fixture, library).await;
            let movie_entry_id = fixture.new_movie_entry(library).await;

            let second = fixture
                .repo()
                .create(CreateMediaFile {
                    library_id: library,
                    path: first.path.clone(),
                    hash: first.hash ^ 1,
                    size_bytes: 2048,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await;

            assert!(second.is_err(), "a second row for a path must be refused");
            let stored = fixture
                .repo()
                .find_by_path(&first.path.to_string_lossy())
                .await
                .unwrap()
                .expect("the first row is still there");
            assert_eq!(stored.id, first.id);
            assert_eq!(stored.hash, first.hash);
        }

        #[tokio::test]
        async fn a_multi_episode_range_is_stored_with_its_first_episode() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let episode_id = fixture.new_episode(library).await;
            let file = file_in(
                &fixture,
                library,
                MediaFileContent::Episode {
                    episode_id,
                    last_episode_number: Some(3),
                },
            )
            .await;

            let stored = fixture
                .repo()
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("the file is present");
            assert!(
                matches!(
                    stored.content,
                    Some(MediaFileContent::Episode {
                        episode_id: id,
                        last_episode_number: Some(3),
                    }) if id == episode_id
                ),
                "{:?}",
                stored.content
            );
        }

        #[tokio::test]
        async fn set_classification_replaces_the_classification_and_nothing_else() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let episode_id = fixture.new_episode(library).await;

            let moved = repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: Some(MediaFileContent::Episode {
                            episode_id,
                            last_episode_number: Some(2),
                        }),
                        status: FileStatus::Known,
                        classifier_version: 7,
                    },
                )
                .await
                .unwrap();
            assert_eq!(moved.id, file.id);
            assert_eq!(moved.classifier_version, 7);
            assert_eq!((moved.hash, moved.size_bytes), (file.hash, file.size_bytes));
            assert_eq!(moved.path, file.path);
            assert_eq!(
                ids(&repo.find_by_episode_id(episode_id).await.unwrap()),
                vec![file.id],
                "the file now belongs to the episode"
            );

            let cleared = repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: None,
                        status: FileStatus::Unknown,
                        classifier_version: 8,
                    },
                )
                .await
                .unwrap();
            assert!(cleared.content.is_none(), "{:?}", cleared.content);
            assert_eq!(cleared.status, FileStatus::Unknown);
            assert_eq!(cleared.classifier_version, 8);
            assert!(repo.find_by_episode_id(episode_id).await.unwrap().is_empty());
            let stored = repo.find_by_id(file.id).await.unwrap().expect("still present");
            assert!(stored.content.is_none());
            assert_eq!(stored.classifier_version, 8);
        }

        /// A file with no content is `Unknown`: `Known` and `Changed` name a
        /// movie's or an episode's file. Every write that would leave a row
        /// otherwise is refused, and a refused update changes nothing.
        #[tokio::test]
        async fn a_file_without_content_can_only_be_unknown() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let create = |status: FileStatus| {
                let unique = Uuid::new_v4();
                CreateMediaFile {
                    library_id: library,
                    path: PathBuf::from(format!("/videos/{library}/{unique}.mkv")),
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status,
                    classifier_version: 0,
                }
            };
            for status in [FileStatus::Known, FileStatus::Changed] {
                assert!(
                    repo.create(create(status)).await.is_err(),
                    "created a {status:?} file with no content"
                );
            }
            let unknown = repo
                .create(create(FileStatus::Unknown))
                .await
                .expect("an Unknown file with no content is valid");

            for status in [FileStatus::Known, FileStatus::Changed] {
                let refused = repo
                    .update(UpdateMediaFile {
                        id: unknown.id,
                        hash: Some(unknown.hash + 1),
                        size_bytes: Some(2048),
                        mtime: None,
                        probe: ProbeUpdate::Keep,
                        content: None,
                        status: Some(status),
                    })
                    .await;
                assert!(refused.is_err(), "updated to {status:?} with no content");
                let refused = repo
                    .set_classification(
                        unknown.id,
                        FileClassification {
                            content: None,
                            status,
                            classifier_version: 1,
                        },
                    )
                    .await;
                assert!(refused.is_err(), "classified {status:?} with no content");
            }
            let stored = repo
                .find_by_id(unknown.id)
                .await
                .unwrap()
                .expect("still present");
            assert_eq!(stored.status, FileStatus::Unknown);
            assert_eq!(
                (stored.hash, stored.size_bytes, stored.classifier_version),
                (unknown.hash, unknown.size_bytes, 0),
                "a refused write changes nothing"
            );
        }

        /// An update sets, keeps or clears a file's probe results -- MIME
        /// type, duration and container format -- together, and clearing
        /// them leaves the rest of the row as it was (issue #181: a failed
        /// probe of changed content must not keep the old content's results).
        #[tokio::test]
        async fn an_update_sets_keeps_or_clears_the_probe_results_together() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let repo = fixture.repo();
            let update = |probe: ProbeUpdate| UpdateMediaFile {
                id: file.id,
                hash: None,
                size_bytes: None,
                mtime: None,
                probe,
                content: None,
                status: None,
            };
            let probe_of = |file: &MediaFile| {
                (
                    file.mime_type.clone(),
                    file.duration,
                    file.container_format.clone(),
                )
            };

            let set = repo
                .update(update(ProbeUpdate::Set {
                    mime_type: "video/mp4".to_string(),
                    duration: ::std::time::Duration::from_secs(90),
                    container_format: "mp4".to_string(),
                }))
                .await
                .expect("set the probe results");
            let expected = (
                Some("video/mp4".to_string()),
                Some(::std::time::Duration::from_secs(90)),
                Some("mp4".to_string()),
            );
            assert_eq!(probe_of(&set), expected);

            let kept = repo
                .update(update(ProbeUpdate::Keep))
                .await
                .expect("keep the probe results");
            assert_eq!(probe_of(&kept), expected);

            repo.update(update(ProbeUpdate::Clear))
                .await
                .expect("clear the probe results");
            let cleared = repo
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("still present");
            assert_eq!(probe_of(&cleared), (None, None, None));
            assert_eq!(
                (
                    cleared.hash,
                    cleared.size_bytes,
                    cleared.status,
                    cleared.content.clone()
                ),
                (file.hash, file.size_bytes, file.status, file.content.clone()),
                "clearing the probe results touches nothing else"
            );
        }

        #[tokio::test]
        async fn a_created_file_is_present() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;

            assert_eq!(file.missing_since, None);
            let found = fixture
                .repo()
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a new file is visible");
            assert_eq!(found.missing_since, None);
        }

        #[tokio::test]
        async fn a_missing_file_is_hidden_from_every_visible_read() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let movie_entry_id = fixture.new_movie_entry(library).await;
            let episode_id = fixture.new_episode(library).await;
            let movie = file_in(
                &fixture,
                library,
                MediaFileContent::Movie { movie_entry_id },
            )
            .await;
            let episode =
                file_in(&fixture, library, MediaFileContent::episode(episode_id)).await;
            let kept = movie_file(&fixture, library).await;

            assert_eq!(
                repo.mark_missing(vec![movie.id, episode.id], at(0))
                    .await
                    .unwrap(),
                2
            );

            assert!(repo.find_by_id(movie.id).await.unwrap().is_none());
            assert!(repo.find_by_id(episode.id).await.unwrap().is_none());
            assert!(repo.find_by_hash(movie.hash).await.unwrap().is_empty());
            assert!(
                repo.find_by_movie_entry_id(movie_entry_id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                repo.find_by_episode_id(episode_id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                ids(&repo.find_all_by_library(library).await.unwrap()),
                vec![kept.id],
                "only the present file is listed"
            );
            // The present file is untouched by its neighbours going missing.
            assert!(repo.find_by_id(kept.id).await.unwrap().is_some());
            assert_eq!(repo.find_by_hash(kept.hash).await.unwrap().len(), 1);
        }

        #[tokio::test]
        async fn a_missing_file_is_still_seen_by_the_reconcile_reads() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let missing = movie_file(&fixture, library).await;
            let present = movie_file(&fixture, library).await;
            repo.mark_missing(vec![missing.id], at(0)).await.unwrap();

            let by_path = repo
                .find_by_path(&missing.path.to_string_lossy())
                .await
                .unwrap()
                .expect("a missing file is still found by its path");
            assert_eq!(by_path.id, missing.id);
            assert_eq!(by_path.missing_since, Some(at(0)));

            let all = repo
                .find_all_by_library_including_missing(library)
                .await
                .unwrap();
            assert_eq!(ids(&all), sorted(vec![missing.id, present.id]));
            let stamped: Vec<Option<DateTime<Utc>>> = all
                .iter()
                .filter(|f| f.id == missing.id)
                .map(|f| f.missing_since)
                .collect();
            assert_eq!(stamped, vec![Some(at(0))]);
        }

        #[tokio::test]
        async fn the_reconcile_listing_is_scoped_to_one_library() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let mine = movie_file(&fixture, library).await;
            let theirs = movie_file(&fixture, other).await;
            repo.mark_missing(vec![mine.id, theirs.id], at(0))
                .await
                .unwrap();

            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                vec![mine.id]
            );
        }

        #[tokio::test]
        async fn marking_a_missing_file_again_keeps_the_first_stamp() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;

            assert_eq!(repo.mark_missing(vec![file.id], at(0)).await.unwrap(), 1);
            assert_eq!(
                repo.mark_missing(vec![file.id], at(3600)).await.unwrap(),
                0,
                "a row already missing is not newly marked"
            );

            let stored = repo
                .find_by_path(&file.path.to_string_lossy())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.missing_since,
                Some(at(0)),
                "the grace period runs from when the file was first found gone"
            );
        }

        #[tokio::test]
        async fn restoring_a_missing_file_keeps_its_id_and_makes_it_visible() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();

            repo.restore(file.id).await.unwrap();

            let found = repo
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a restored file is visible again under its old id");
            assert_eq!(found.missing_since, None);
            assert_eq!(found.path, file.path);
            assert_eq!(
                ids(&repo.find_all_by_library(library).await.unwrap()),
                vec![file.id]
            );
        }

        #[tokio::test]
        async fn a_restored_file_that_goes_missing_again_takes_a_fresh_stamp() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();
            repo.restore(file.id).await.unwrap();

            assert_eq!(repo.mark_missing(vec![file.id], at(60)).await.unwrap(), 1);

            let stored = repo
                .find_by_path(&file.path.to_string_lossy())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.missing_since, Some(at(60)));
        }

        #[tokio::test]
        async fn purge_removes_only_the_listed_rows_that_are_missing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let missing = movie_file(&fixture, library).await;
            let present = movie_file(&fixture, library).await;
            let unlisted = movie_file(&fixture, library).await;
            repo.mark_missing(vec![missing.id, unlisted.id], at(0))
                .await
                .unwrap();

            assert_eq!(
                repo.purge_missing(vec![missing.id, present.id])
                    .await
                    .unwrap(),
                1,
                "a present file is never purged, even when listed"
            );

            assert!(
                repo.find_by_path(&missing.path.to_string_lossy())
                    .await
                    .unwrap()
                    .is_none(),
                "the purged row is gone from the reconcile reads too"
            );
            assert!(repo.find_by_id(present.id).await.unwrap().is_some());
            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                sorted(vec![present.id, unlisted.id]),
                "an unlisted missing row is left for its own grace period"
            );
        }

        /// A file of a new title under `library_id` whose content hash is
        /// `hash`, at a fresh path.
        async fn file_with_hash(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            hash: u64,
        ) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path: PathBuf::from(format!("/videos/{library_id}/{}.mkv", Uuid::new_v4())),
                    hash,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .expect("create a file")
        }

        /// Point `row` at `path`, found there at `size_bytes` bytes and `mtime`.
        fn to(
            row: &MediaFile,
            path: &::std::path::Path,
            size_bytes: u64,
            mtime: Option<DateTime<Utc>>,
        ) -> FileRelink {
            FileRelink {
                id: row.id,
                path: path.to_path_buf(),
                size_bytes,
                mtime,
            }
        }

        /// The row now stored at `path`, if any.
        async fn at_path(fixture: &impl FileRepositoryFixture, path: &::std::path::Path) -> Option<MediaFile> {
            fixture
                .repo()
                .find_by_path(&path.to_string_lossy())
                .await
                .unwrap()
        }

        /// A moved file keeps its row (issue #180): the relink points it at
        /// the new path, records what was found there, and brings it back if
        /// it had been marked missing -- keeping its id, hash and title.
        #[tokio::test]
        async fn relink_moves_a_row_to_a_new_path_keeping_its_id_and_title() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();
            let moved_to = PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));

            repo.relink(vec![to(&file, &moved_to, 4096, Some(at(30)))], Vec::new(), at(60))
                .await
                .expect("relink the row");

            let stored = repo
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a relinked file is visible again");
            assert_eq!(stored.path, moved_to);
            assert_eq!(
                (stored.size_bytes, stored.mtime, stored.missing_since),
                (4096, Some(at(30)), None)
            );
            assert_eq!(
                (stored.hash, stored.content.clone(), stored.status),
                (file.hash, file.content.clone(), file.status),
                "the content and its title are kept"
            );
            assert!(
                at_path(&fixture, &file.path).await.is_none(),
                "nothing is left at the old path"
            );
            assert_eq!(at_path(&fixture, &moved_to).await.map(|f| f.id), Some(file.id));
        }

        /// Two files that swapped names swap paths in one relink, although
        /// neither path is free until the other row has left it: one row per
        /// path holds when the relink is done, not step by step.
        #[tokio::test]
        async fn two_rows_swap_paths_in_one_relink() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;

            fixture
                .repo()
                .relink(
                    vec![
                        to(&heat, &ronin.path, 7, Some(at(1))),
                        to(&ronin, &heat.path, 9, Some(at(2))),
                    ],
                    Vec::new(),
                    at(60),
                )
                .await
                .expect("the rows swap paths");

            let now_heat = at_path(&fixture, &ronin.path).await.unwrap();
            let now_ronin = at_path(&fixture, &heat.path).await.unwrap();
            assert_eq!((now_heat.id, now_heat.size_bytes), (heat.id, 7));
            assert_eq!((now_ronin.id, now_ronin.size_bytes), (ronin.id, 9));
            assert_eq!(now_heat.content, heat.content, "each keeps its title");
        }

        /// A rotation moves two rows along and displaces the third, whose
        /// content is nowhere: it is kept, missing as of the relink, at its
        /// displaced path -- which leaves its old path to the row moving in.
        /// A displaced row already missing keeps its first stamp.
        #[tokio::test]
        async fn a_displaced_row_is_kept_aside_as_missing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let (a, b, c) = (
                movie_file(&fixture, library).await,
                movie_file(&fixture, library).await,
                movie_file(&fixture, library).await,
            );
            let stale = movie_file(&fixture, library).await;
            let d = movie_file(&fixture, library).await;
            repo.mark_missing(vec![stale.id], at(0)).await.unwrap();

            repo.relink(
                vec![
                    to(&a, &b.path, 1, None),
                    to(&c, &a.path, 1, None),
                    to(&d, &stale.path, 1, None),
                ],
                vec![b.id, stale.id],
                at(60),
            )
            .await
            .expect("rotate");

            assert_eq!(at_path(&fixture, &b.path).await.map(|f| f.id), Some(a.id));
            assert_eq!(at_path(&fixture, &a.path).await.map(|f| f.id), Some(c.id));
            assert_eq!(at_path(&fixture, &stale.path).await.map(|f| f.id), Some(d.id));
            assert!(at_path(&fixture, &c.path).await.is_none());

            let aside = at_path(&fixture, &displaced_path(&b.path, b.id))
                .await
                .expect("the displaced row is kept");
            assert_eq!(aside.id, b.id);
            assert_eq!(aside.missing_since, Some(at(60)));
            assert_eq!(aside.content, b.content, "with its title");
            let stale_aside = at_path(&fixture, &displaced_path(&stale.path, stale.id))
                .await
                .unwrap();
            assert_eq!(stale_aside.missing_since, Some(at(0)), "the first stamp");
        }

        /// One row per path holds for a relink too: moving a row onto a path
        /// a row outside the relink holds is refused, and no row changes --
        /// not even those the same call would have moved.
        #[tokio::test]
        async fn relinking_onto_a_path_another_row_holds_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;
            let moving = movie_file(&fixture, library).await;
            let holder = movie_file(&fixture, library).await;
            repo.mark_missing(vec![moving.id], at(0)).await.unwrap();

            let refused = repo
                .relink(
                    vec![
                        to(&heat, &ronin.path, 1, None),
                        to(&ronin, &heat.path, 1, None),
                        to(&moving, &holder.path, 1, Some(at(9))),
                    ],
                    Vec::new(),
                    at(60),
                )
                .await;

            assert!(refused.is_err(), "the path is taken");
            for row in [&heat, &ronin, &moving, &holder] {
                let stored = at_path(&fixture, &row.path)
                    .await
                    .expect("every row is where it was");
                assert_eq!(stored.id, row.id);
                assert_eq!(stored.missing_since, row.missing_since.or(
                    (row.id == moving.id).then(|| at(0))
                ));
            }
        }

        /// Two rows cannot be relinked to one path, and one row cannot be
        /// named twice.
        #[tokio::test]
        async fn a_relink_naming_a_path_or_a_row_twice_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;
            let free = PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4()));

            let one_path = repo
                .relink(
                    vec![to(&heat, &free, 1, None), to(&ronin, &free, 1, None)],
                    Vec::new(),
                    at(60),
                )
                .await;
            let one_row = repo
                .relink(vec![to(&heat, &free, 1, None)], vec![heat.id], at(60))
                .await;

            assert!(one_path.is_err() && one_row.is_err());
            assert!(at_path(&fixture, &free).await.is_none());
            assert_eq!(at_path(&fixture, &heat.path).await.map(|f| f.id), Some(heat.id));
            assert_eq!(at_path(&fixture, &ronin.path).await.map(|f| f.id), Some(ronin.id));
        }

        #[tokio::test]
        async fn relinking_a_row_that_does_not_exist_fails_and_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let path = PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4()));
            let ghost = MediaFile {
                id: Uuid::new_v4(),
                ..heat.clone()
            };

            assert!(
                repo.relink(vec![to(&ghost, &path, 1, None)], Vec::new(), at(60))
                    .await
                    .is_err()
            );
            assert!(
                repo.relink(Vec::new(), vec![ghost.id, heat.id], at(60))
                    .await
                    .is_err()
            );
            assert!(at_path(&fixture, &path).await.is_none());
            let stored = at_path(&fixture, &heat.path).await.expect("left where it was");
            assert_eq!(stored.missing_since, None);
        }

        /// The relink lookup sees a missing row -- the moved file's -- and
        /// never a row of another library, whatever its hash.
        #[tokio::test]
        async fn the_hash_lookup_includes_missing_rows_of_one_library_only() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            // Positive and unique per run: the hash is a signed BIGINT column.
            let hash = (Uuid::new_v4().as_u128() as u64) >> 1;
            let present = file_with_hash(&fixture, library, hash).await;
            let missing = file_with_hash(&fixture, library, hash).await;
            let elsewhere = file_with_hash(&fixture, other, hash).await;
            let different = file_with_hash(&fixture, library, hash ^ 1).await;
            repo.mark_missing(vec![missing.id, elsewhere.id], at(0))
                .await
                .unwrap();

            let found = repo
                .find_by_library_and_hash_including_missing(library, hash)
                .await
                .unwrap();

            assert_eq!(ids(&found), sorted(vec![present.id, missing.id]));
            assert!(!ids(&found).contains(&different.id));
        }

        /// A file of a new title under `library_id` at `path`.
        async fn file_at(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            path: PathBuf,
        ) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path,
                    hash: 1,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .expect("create a file")
        }

        /// The rows beneath a directory are those whose path continues it
        /// by whole components -- missing ones included, another library's
        /// never. A `_` or `%` in the directory's name is itself, not a
        /// wildcard.
        #[tokio::test]
        async fn the_rows_beneath_a_directory_are_matched_by_whole_components() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let base = PathBuf::from(format!("/videos/{library}/{}", Uuid::new_v4()));
            let dir = base.join("Show_%1");
            let direct = file_at(&fixture, library, dir.join("a.mkv")).await;
            let nested = file_at(&fixture, library, dir.join("S01/b.mkv")).await;
            repo.mark_missing(vec![nested.id], at(0)).await.unwrap();
            // Longer by a character, and what the wildcards would match.
            file_at(&fixture, library, base.join("Show_%10/c.mkv")).await;
            file_at(&fixture, library, base.join("ShowAB1/d.mkv")).await;
            file_at(&fixture, library, base.join("Show_%1.mkv")).await;
            file_at(&fixture, other, dir.join("e.mkv")).await;

            let found = repo
                .find_beneath_including_missing(library, &dir)
                .await
                .unwrap();

            assert_eq!(ids(&found), sorted(vec![direct.id, nested.id]));
        }

        #[tokio::test]
        async fn empty_id_lists_change_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let gone = movie_file(&fixture, library).await;
            repo.mark_missing(vec![gone.id], at(0)).await.unwrap();

            assert_eq!(repo.mark_missing(Vec::new(), at(60)).await.unwrap(), 0);
            assert_eq!(repo.purge_missing(Vec::new()).await.unwrap(), 0);

            assert!(repo.find_by_id(file.id).await.unwrap().is_some());
            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                sorted(vec![file.id, gone.id])
            );
        }
    };
}

/// Behavioural contract for [`crate::repositories::ShowRepository`].
///
/// `$setup` names an `async fn() -> impl ShowRepositoryFixture`.
#[macro_export]
macro_rules! show_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFile, MediaFileContent};
        use $crate::models::show::{CreateEpisode, CreateShow};
        use $crate::providers::enrichment::ShowEnrichment;
        use $crate::repositories::ShowRepository;
        use $crate::repositories::contract::fixture::ShowRepositoryFixture;
        use $crate::utils::media_path::CLASSIFIER_VERSION;

        /// A show parsed as a title of its own; a fresh UUID keeps tests
        /// apart.
        fn new_show(name: &str) -> CreateShow {
            CreateShow::new(format!("{name} {}", Uuid::new_v4()), None)
        }

        /// A show of its own -- the title carries a fresh UUID so parallel
        /// tests against one database never share a show -- and its seasons
        /// `1` and `2`.
        async fn new_seasons(repo: &dyn ShowRepository) -> (Uuid, Uuid) {
            let show = repo
                .find_or_create_by_identity(CreateShow::new(
                    format!("contract show {}", Uuid::new_v4()),
                    None,
                ))
                .await
                .unwrap();
            let one = repo.find_or_create_season(show.id, 1).await.unwrap();
            let two = repo.find_or_create_season(show.id, 2).await.unwrap();
            (one.id, two.id)
        }

        /// A present file for `episode_id`, in a library of its own.
        async fn episode_file(fixture: &impl ShowRepositoryFixture, episode_id: Uuid) -> MediaFile {
            let library_id = fixture.new_library().await;
            let unique = Uuid::new_v4();
            fixture
                .files()
                .create(CreateMediaFile {
                    library_id,
                    path: ::std::path::PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
                    // Positive and unique: the hash is a signed BIGINT column.
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(MediaFileContent::episode(episode_id)),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .expect("create an episode file")
        }

        /// A cutoff every row created so far falls before.
        fn after_everything() -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::Utc::now() + ::chrono::Duration::minutes(1)
        }

        /// Runtimes are whole minutes: the SQL schema stores `runtime_mins`.
        fn episode(season_id: Uuid, episode_number: u32, title: &str, mins: u64) -> CreateEpisode {
            CreateEpisode {
                season_id,
                episode_number,
                title: title.to_string(),
                runtime: Some(Duration::from_secs(mins * 60)),
                air_date: None,
            }
        }

        #[tokio::test]
        async fn a_new_episode_keeps_its_air_date() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season, _) = new_seasons(repo).await;
            let aired = ::chrono::NaiveDate::from_ymd_opt(2024, 3, 1).expect("a valid date");

            let created = repo
                .find_or_create_episode(CreateEpisode {
                    air_date: Some(aired),
                    ..episode(season, 301, "Guest", 30)
                })
                .await
                .unwrap();

            let stored = repo
                .find_episode_by_id(created.id)
                .await
                .unwrap()
                .expect("the episode is readable by id");
            assert_eq!(stored.air_date, Some(aired.to_string()));
        }

        /// Episodes of one new season indexed at once -- two libraries'
        /// scans -- share one season row rather than failing on the unique
        /// index (issue #181).
        #[tokio::test]
        async fn concurrent_find_or_create_season_calls_share_one_row() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let show = repo
                .find_or_create_by_identity(new_show("seasons"))
                .await
                .unwrap();

            let call = || repo.find_or_create_season(show.id, 3);
            let (a, b, c, d, e, f, g, h) = ::tokio::join!(
                call(),
                call(),
                call(),
                call(),
                call(),
                call(),
                call(),
                call()
            );
            let ids: ::std::collections::HashSet<Uuid> = [a, b, c, d, e, f, g, h]
                .into_iter()
                .map(|season| season.expect("no call fails on the unique index").id)
                .collect();

            assert_eq!(ids.len(), 1, "every call returns the same season");
            assert_eq!(
                repo.find_seasons_by_show_id(show.id).await.unwrap().len(),
                1,
                "one row was inserted"
            );
        }

        #[tokio::test]
        async fn find_or_create_episode_returns_the_same_row_for_the_same_pair() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season, _) = new_seasons(repo).await;

            let first = repo
                .find_or_create_episode(episode(season, 1, "Pilot", 45))
                .await
                .unwrap();
            let second = repo
                .find_or_create_episode(episode(season, 1, "Pilot", 45))
                .await
                .unwrap();

            assert_eq!(first.id, second.id, "one logical episode per pair");
            assert_eq!(
                repo.find_episodes_by_season_id(season).await.unwrap().len(),
                1,
                "no second row was inserted"
            );
        }

        #[tokio::test]
        async fn find_or_create_episode_leaves_an_existing_episode_unchanged() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season, _) = new_seasons(repo).await;

            let first = repo
                .find_or_create_episode(episode(season, 3, "The Original Title", 45))
                .await
                .unwrap();
            let again = repo
                .find_or_create_episode(episode(season, 3, "some.other.rip.720p", 52))
                .await
                .unwrap();

            assert_eq!(again.id, first.id);
            assert_eq!(again.title, "The Original Title");
            assert_eq!(again.runtime, Some(Duration::from_secs(45 * 60)));

            let stored = repo
                .find_episode_by_id(first.id)
                .await
                .unwrap()
                .expect("the episode is readable by id");
            assert_eq!(
                stored.title, "The Original Title",
                "a later file's parse must not rewrite the stored title"
            );
            assert_eq!(stored.runtime, Some(Duration::from_secs(45 * 60)));
        }

        #[tokio::test]
        async fn find_or_create_episode_keeps_distinct_episode_numbers_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season, _) = new_seasons(repo).await;

            let one = repo
                .find_or_create_episode(episode(season, 1, "One", 30))
                .await
                .unwrap();
            let two = repo
                .find_or_create_episode(episode(season, 2, "Two", 30))
                .await
                .unwrap();

            assert_ne!(one.id, two.id);
            let numbers: Vec<u32> = repo
                .find_episodes_by_season_id(season)
                .await
                .unwrap()
                .into_iter()
                .map(|e| e.episode_number)
                .collect();
            assert_eq!(numbers, vec![1, 2]);
        }

        #[tokio::test]
        async fn find_or_create_episode_keeps_one_number_in_different_seasons_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season_one, season_two) = new_seasons(repo).await;

            let s01e01 = repo
                .find_or_create_episode(episode(season_one, 1, "S01E01", 30))
                .await
                .unwrap();
            let s02e01 = repo
                .find_or_create_episode(episode(season_two, 1, "S02E01", 30))
                .await
                .unwrap();

            assert_ne!(s01e01.id, s02e01.id);
            assert_eq!(s01e01.season_id, season_one);
            assert_eq!(s02e01.season_id, season_two);
            assert_eq!(s02e01.title, "S02E01");
        }

        #[tokio::test]
        async fn find_or_create_show_by_identity_returns_one_unchanged_row_per_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let name = format!("Doctor Who {}", Uuid::new_v4());

            let first = repo
                .find_or_create_by_identity(CreateShow::new(name.clone(), Some(2005)))
                .await
                .unwrap();
            // Another spelling of the same series folder: same key.
            let mut respelled = CreateShow::new(name.to_uppercase().replace(' ', "."), Some(2005));
            respelled.title = "some other folder spelling".to_string();
            let again = repo.find_or_create_by_identity(respelled).await.unwrap();
            let other_year = repo
                .find_or_create_by_identity(CreateShow::new(name.clone(), Some(1963)))
                .await
                .unwrap();

            assert_eq!(again.id, first.id, "one show per identity key");
            assert_eq!(again.title, name, "an existing show is returned unchanged");
            assert_ne!(
                other_year.id, first.id,
                "a different year is a different show"
            );
        }

        #[tokio::test]
        async fn an_enriched_show_is_still_found_by_its_folder_key() {
            // Issue #183: enrichment used to rewrite the column the indexer
            // matched on, so the next episode of a renamed show made a second
            // show.
            let fixture = $setup().await;
            let repo = fixture.repo();
            let parsed = CreateShow::new(format!("Shogun {}", Uuid::new_v4()), None);
            let show = repo
                .find_or_create_by_identity(parsed.clone())
                .await
                .unwrap();

            repo.apply_enrichment(
                show.id,
                &ShowEnrichment {
                    title: "Shōgun (Provider Title)".to_string(),
                    year: Some(2024),
                    rating: Some(8.5),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let found = repo.find_or_create_by_identity(parsed).await.unwrap();
            assert_eq!(found.id, show.id, "the renamed show is still found");
            assert_eq!(
                found.title, "Shōgun (Provider Title)",
                "enrichment's title stands"
            );
            assert_eq!(
                found.rating_tmdb,
                Some(8.5),
                "enrichment's rating is kept (issue #187)"
            );
            assert_eq!(found.identity_key, show.identity_key);
        }

        #[tokio::test]
        async fn find_by_ids_reads_every_named_show_in_one_call() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let one = repo
                .find_or_create_by_identity(new_show("One"))
                .await
                .unwrap();
            let two = repo
                .find_or_create_by_identity(new_show("Two"))
                .await
                .unwrap();

            let mut found: Vec<Uuid> = repo
                .find_by_ids(&[two.id, Uuid::new_v4(), one.id])
                .await
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
            found.sort();
            let mut expected = vec![one.id, two.id];
            expected.sort();
            assert_eq!(found, expected, "an unknown id is skipped");
            assert!(repo.find_by_ids(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn child_counts_count_every_season_and_episode_per_show() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (one, two) = new_seasons(repo).await;
            let show = repo.find_season_by_id(one).await.unwrap().unwrap().show_id;
            repo.find_or_create_episode(episode(one, 1, "Pilot", 30))
                .await
                .unwrap();
            repo.find_or_create_episode(episode(one, 2, "Second", 30))
                .await
                .unwrap();
            repo.find_or_create_episode(episode(two, 1, "Return", 30))
                .await
                .unwrap();
            let bare = repo
                .find_or_create_by_identity(new_show("Bare"))
                .await
                .unwrap();
            let (empty_season, _) = new_seasons(repo).await;
            let seasons_only = repo
                .find_season_by_id(empty_season)
                .await
                .unwrap()
                .unwrap()
                .show_id;

            let counts = repo
                .child_counts(&[show, bare.id, seasons_only])
                .await
                .unwrap();

            assert_eq!(
                counts.get(&show).copied(),
                Some($crate::models::catalog::ShowChildCounts {
                    seasons: 2,
                    episodes: 3
                })
            );
            assert_eq!(
                counts.get(&seasons_only).copied(),
                Some($crate::models::catalog::ShowChildCounts {
                    seasons: 2,
                    episodes: 0
                }),
                "a season without episodes still counts"
            );
            assert!(
                !counts.contains_key(&bare.id),
                "a show with no season is absent"
            );
            assert!(repo.child_counts(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn delete_orphaned_removes_fileless_episodes_seasons_and_shows() {
            let fixture = $setup().await;
            let repo = fixture.repo();

            let (kept_season, emptied_season) = new_seasons(repo).await;
            let kept_show = repo
                .find_season_by_id(kept_season)
                .await
                .unwrap()
                .unwrap()
                .show_id;
            let watched = repo
                .find_or_create_episode(episode(kept_season, 1, "Watched", 30))
                .await
                .unwrap();
            episode_file(&fixture, watched.id).await;
            let fileless = repo
                .find_or_create_episode(episode(kept_season, 2, "Fileless", 30))
                .await
                .unwrap();
            repo.find_or_create_episode(episode(emptied_season, 1, "Also fileless", 30))
                .await
                .unwrap();

            let (gone_season, _) = new_seasons(repo).await;
            let gone_show = repo
                .find_season_by_id(gone_season)
                .await
                .unwrap()
                .unwrap()
                .show_id;
            repo.find_or_create_episode(episode(gone_season, 1, "Nothing", 30))
                .await
                .unwrap();

            // Everything was created after this cutoff: nothing is old enough.
            let created = repo
                .find_by_id(kept_show)
                .await
                .unwrap()
                .unwrap()
                .created_at;
            assert_eq!(
                repo.delete_orphaned(created - ::chrono::Duration::seconds(1))
                    .await
                    .unwrap(),
                0
            );
            assert!(repo.find_by_id(gone_show).await.unwrap().is_some());

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 1);

            assert!(repo.find_by_id(gone_show).await.unwrap().is_none());
            assert!(repo.find_by_id(kept_show).await.unwrap().is_some());
            assert!(repo.find_episode_by_id(watched.id).await.unwrap().is_some());
            assert!(
                repo.find_episode_by_id(fileless.id)
                    .await
                    .unwrap()
                    .is_none()
            );
            let seasons: Vec<u32> = repo
                .find_seasons_by_show_id(kept_show)
                .await
                .unwrap()
                .into_iter()
                .map(|s| s.season_number)
                .collect();
            assert_eq!(seasons, vec![1], "the season left without episodes goes");
        }

        #[tokio::test]
        async fn a_soft_deleted_episode_file_keeps_its_show_until_it_is_purged() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (season, _) = new_seasons(repo).await;
            let show = repo
                .find_season_by_id(season)
                .await
                .unwrap()
                .unwrap()
                .show_id;
            let ep = repo
                .find_or_create_episode(episode(season, 1, "Pilot", 30))
                .await
                .unwrap();
            let file = episode_file(&fixture, ep.id).await;
            fixture
                .files()
                .mark_missing(vec![file.id], ::chrono::Utc::now())
                .await
                .unwrap();

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 0);
            assert!(repo.find_by_id(show).await.unwrap().is_some());

            fixture.files().purge_missing(vec![file.id]).await.unwrap();
            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 1);
            assert!(repo.find_by_id(show).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn an_unkeyed_show_is_never_matched_and_can_be_keyed_once() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = format!("Legacy Show {}", Uuid::new_v4());
            let legacy = fixture.new_unkeyed_show(&title, ::chrono::Utc::now()).await;
            let parsed = CreateShow::new(title.clone(), None);

            assert!(
                repo.find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|s| s.id == legacy)
            );

            assert!(
                repo.assign_identity_key(legacy, &parsed.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|s| s.id == legacy),
                "a keyed show is no longer unkeyed"
            );
            assert_eq!(
                repo.find_or_create_by_identity(parsed).await.unwrap().id,
                legacy,
                "once keyed, the legacy show is found by its key"
            );
            assert!(
                !repo
                    .assign_identity_key(legacy, "another key|", CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a key is assigned once, never replaced"
            );
        }

        #[tokio::test]
        async fn unkeyed_shows_are_listed_oldest_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            // Whole seconds: Postgres keeps microseconds, the double keeps
            // nanoseconds, and the contract must not depend on either.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let title = format!("Legacy Show {}", Uuid::new_v4());
            // Inserted newest first, so insertion order is not the answer.
            let newest = fixture
                .new_unkeyed_show(&title, base + ::chrono::Duration::days(2))
                .await;
            let oldest = fixture.new_unkeyed_show(&title, base).await;
            let middle = fixture
                .new_unkeyed_show(&title, base + ::chrono::Duration::days(1))
                .await;

            let listed: Vec<Uuid> = repo
                .find_unkeyed()
                .await
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .filter(|id| [newest, oldest, middle].contains(id))
                .collect();
            assert_eq!(
                listed,
                vec![oldest, middle, newest],
                "of legacy duplicates, the original comes first"
            );
        }

        #[tokio::test]
        async fn an_unkeyed_show_is_not_matched_and_cannot_take_a_held_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = format!("Duplicate Show {}", Uuid::new_v4());
            let legacy = fixture.new_unkeyed_show(&title, ::chrono::Utc::now()).await;

            let keyed = repo
                .find_or_create_by_identity(CreateShow::new(title.clone(), None))
                .await
                .unwrap();
            assert_ne!(keyed.id, legacy, "a keyless row is never matched");

            assert!(
                !repo
                    .assign_identity_key(
                        legacy,
                        keyed.identity_key.as_deref().unwrap(),
                        CLASSIFIER_VERSION
                    )
                    .await
                    .unwrap(),
                "a key another show holds is refused"
            );
            assert_eq!(
                repo.find_by_id(legacy).await.unwrap().unwrap().identity_key,
                None
            );
        }

        #[tokio::test]
        async fn a_show_key_records_the_rules_version_that_derived_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let current = repo
                .find_or_create_by_identity(new_show("Current"))
                .await
                .unwrap();
            let stale = repo
                .find_or_create_by_identity(CreateShow {
                    identity_key_version: 0,
                    ..new_show("Stale")
                })
                .await
                .unwrap();
            // Whole seconds, as in the oldest-first test above.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let legacy = new_show("Legacy");
            let older = fixture.new_unkeyed_show(&legacy.title, base).await;
            assert!(
                repo.assign_identity_key(older, &legacy.identity_key, 0)
                    .await
                    .unwrap()
            );
            let fresh = new_show("Fresh");
            let fresh_legacy = fixture.new_unkeyed_show(&fresh.title, base).await;
            assert!(
                repo.assign_identity_key(fresh_legacy, &fresh.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            let unkeyed = fixture
                .new_unkeyed_show(&format!("Unkeyed {}", Uuid::new_v4()), base)
                .await;
            let ours = [current.id, stale.id, older, fresh_legacy, unkeyed];

            let listed = |version: u16| async move {
                repo.find_keyed_before_version(version)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|t| t.id)
                    .filter(|id| ours.contains(id))
                    .collect::<Vec<Uuid>>()
            };
            assert_eq!(
                listed(CLASSIFIER_VERSION).await,
                vec![older, stale.id],
                "keys older rules derived, oldest first; never a keyless row"
            );
            let mut next = listed(CLASSIFIER_VERSION + 1).await;
            next.sort();
            let mut keyed = vec![current.id, stale.id, older, fresh_legacy];
            keyed.sort();
            assert_eq!(next, keyed, "every key is older than rules not yet written");
        }

        #[tokio::test]
        async fn a_show_is_found_by_its_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let created = repo
                .find_or_create_by_identity(new_show("Keyed"))
                .await
                .unwrap();
            let key = created.identity_key.clone().unwrap();

            assert_eq!(
                repo.find_by_identity_key(&key).await.unwrap().map(|t| t.id),
                Some(created.id)
            );
            assert!(
                repo.find_by_identity_key(&format!("nobody {}|", Uuid::new_v4()))
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn rekeying_a_show_replaces_its_key_and_keeps_the_show() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let old = CreateShow {
                identity_key_version: 0,
                ..new_show("Old Spelling")
            };
            let created = repo.find_or_create_by_identity(old.clone()).await.unwrap();
            repo.apply_enrichment(
                created.id,
                &ShowEnrichment {
                    title: "Provider Title".to_string(),
                    tmdb_id: Some(42),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let new = new_show("New Spelling");

            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap()
            );
            let stored = repo
                .find_by_identity_key(&new.identity_key)
                .await
                .unwrap()
                .expect("found by its new key");
            assert_eq!(stored.id, created.id, "the same row, not a new one");
            assert_eq!(stored.tmdb_id, Some(42), "with its enrichment");
            assert!(
                repo.find_by_identity_key(&old.identity_key)
                    .await
                    .unwrap()
                    .is_none(),
                "the old key is free"
            );
            assert!(
                !repo
                    .find_keyed_before_version(CLASSIFIER_VERSION)
                    .await
                    .unwrap()
                    .iter()
                    .any(|t| t.id == created.id),
                "the new key carries the version that derived it"
            );
            assert_eq!(
                repo.find_or_create_by_identity(new.clone())
                    .await
                    .unwrap()
                    .id,
                created.id,
                "the next file of the new spelling finds it"
            );
            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap(),
                "a key it already holds is no clash"
            );
        }

        #[tokio::test]
        async fn rekeying_a_show_refuses_a_held_key_and_can_release_its_own() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let a = repo
                .find_or_create_by_identity(new_show("A"))
                .await
                .unwrap();
            let b = repo
                .find_or_create_by_identity(new_show("B"))
                .await
                .unwrap();
            let b_key = b.identity_key.clone().unwrap();

            assert!(
                !repo
                    .rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "another title holds it"
            );
            assert_eq!(
                repo.find_by_id(a.id).await.unwrap().unwrap().identity_key,
                a.identity_key,
                "a refused rekey changes nothing"
            );

            assert!(repo.rekey(b.id, None, CLASSIFIER_VERSION).await.unwrap());
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().identity_key,
                None,
                "released: never matched again"
            );
            assert!(
                repo.rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a released key is free"
            );
            assert!(
                !repo
                    .rekey(
                        Uuid::new_v4(),
                        Some("nobody|".to_string()),
                        CLASSIFIER_VERSION
                    )
                    .await
                    .unwrap(),
                "an unknown title is not rekeyed"
            );
        }
    };
}

/// Behavioural contract for [`crate::repositories::MovieRepository`]: one
/// movie per identity key whatever enrichment does to the display title
/// (issue #183), search lists only movies with a present file, and orphaned
/// movies are deleted only once no file row is left.
///
/// `$setup` names an `async fn() -> impl MovieRepositoryFixture`.
#[macro_export]
macro_rules! movie_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFile, MediaFileContent};
        use $crate::models::movie::{CreateMovie, CreateMovieEntry, Movie};
        use $crate::providers::enrichment::MovieEnrichment;
        use $crate::repositories::MovieRepository;
        use $crate::repositories::contract::fixture::MovieRepositoryFixture;
        use $crate::utils::media_path::CLASSIFIER_VERSION;

        /// A movie parsed as a title of its own -- a fresh UUID keeps tests
        /// apart -- released in `year`.
        fn parsed(name: &str, year: Option<u32>) -> CreateMovie {
            CreateMovie::new(format!("{name} {}", Uuid::new_v4()), year, None)
        }

        /// [`parsed`] with no year.
        fn new_movie(name: &str) -> CreateMovie {
            parsed(name, None)
        }

        /// Give `movie` an entry, and -- when `with_file` -- a present file
        /// behind it, returning the file.
        async fn entry_for(
            fixture: &impl MovieRepositoryFixture,
            movie: &Movie,
            with_file: bool,
        ) -> Option<MediaFile> {
            let library_id = fixture.new_library().await;
            let entry = fixture
                .repo()
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                    is_primary: true,
                })
                .await
                .expect("create an entry");
            if !with_file {
                return None;
            }
            let unique = Uuid::new_v4();
            Some(
                fixture
                    .files()
                    .create(CreateMediaFile {
                        library_id,
                        path: ::std::path::PathBuf::from(format!(
                            "/videos/{library_id}/{unique}.mkv"
                        )),
                        hash: (unique.as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        mime_type: Some("video/x-matroska".to_string()),
                        duration: None,
                        container_format: Some("matroska".to_string()),
                        content: Some(MediaFileContent::Movie {
                            movie_entry_id: entry.id,
                        }),
                        status: FileStatus::Known,
                        classifier_version: 0,
                    })
                    .await
                    .expect("create a movie file"),
            )
        }

        fn after_everything() -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::Utc::now() + ::chrono::Duration::minutes(1)
        }

        #[tokio::test]
        async fn find_or_create_entry_returns_one_entry_per_library_movie_and_edition() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(parsed("Editions", Some(2019)))
                .await
                .unwrap();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let entry = |library_id: Uuid, edition: Option<&str>| CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: edition.map(str::to_string),
                is_primary: true,
            };

            let default = repo
                .find_or_create_entry(entry(library, None))
                .await
                .unwrap();
            let again = repo
                .find_or_create_entry(entry(library, None))
                .await
                .unwrap();
            assert_eq!(
                again.id, default.id,
                "a second copy of the default edition shares its entry"
            );

            let cut = repo
                .find_or_create_entry(entry(library, Some("Director's Cut")))
                .await
                .unwrap();
            let cut_again = repo
                .find_or_create_entry(entry(library, Some("Director's Cut")))
                .await
                .unwrap();
            assert_eq!(cut_again.id, cut.id);
            assert_ne!(cut.id, default.id, "an edition is its own entry");
            assert_eq!(cut.edition.as_deref(), Some("Director's Cut"));

            let elsewhere = repo
                .find_or_create_entry(entry(other_library, None))
                .await
                .unwrap();
            assert_ne!(elsewhere.id, default.id, "another library is another entry");

            assert_eq!(
                repo.find_entries_by_movie_id(movie.id).await.unwrap().len(),
                3
            );
        }

        #[tokio::test]
        async fn find_or_create_by_identity_returns_one_unchanged_row_per_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Amelie", Some(2001));

            let first = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();
            let mut later = create.clone();
            later.title = "Amélie.2001.1080p".to_string();
            later.runtime = Some(::std::time::Duration::from_secs(90 * 60));
            let again = repo.find_or_create_by_identity(later).await.unwrap();

            assert_eq!(again.id, first.id, "one movie per identity key");
            assert_eq!(
                again.title, create.title,
                "an existing movie is returned unchanged"
            );
            assert_eq!(again.runtime, None);
            assert_eq!(
                first.identity_key.as_deref(),
                Some(create.identity_key.as_str())
            );
        }

        #[tokio::test]
        async fn movies_of_one_title_and_different_years_are_different_movies() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let dune = format!("Dune {}", Uuid::new_v4());

            let lynch = repo
                .find_or_create_by_identity(CreateMovie::new(dune.clone(), Some(1984), None))
                .await
                .unwrap();
            let villeneuve = repo
                .find_or_create_by_identity(CreateMovie::new(dune.clone(), Some(2021), None))
                .await
                .unwrap();

            assert_ne!(lynch.id, villeneuve.id);
        }

        #[tokio::test]
        async fn an_enriched_movie_is_still_found_by_its_original_key() {
            // Issue #183: enrichment used to rewrite the column the indexer
            // matched on, so the renamed movie's next file made a duplicate
            // that then failed enrichment on the unique `tmdb_id`.
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Leon", Some(1994));
            let movie = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();

            repo.apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: "Léon: The Professional".to_string(),
                    year: Some(1995),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let found = repo.find_or_create_by_identity(create).await.unwrap();
            assert_eq!(found.id, movie.id, "the renamed movie is still found");
            assert_eq!(
                found.title, "Léon: The Professional",
                "enrichment's title stands"
            );
            assert_eq!(found.year, Some(1995));
            assert_eq!(
                found.identity_key, movie.identity_key,
                "the key is untouched"
            );
        }

        #[tokio::test]
        async fn find_by_ids_reads_every_named_movie_in_one_call() {
            let fixture = $setup().await;
            let repo: &dyn MovieRepository = fixture.repo();
            let one = repo
                .find_or_create_by_identity(new_movie("One"))
                .await
                .unwrap();
            let two = repo
                .find_or_create_by_identity(new_movie("Two"))
                .await
                .unwrap();

            let mut found: Vec<Uuid> = repo
                .find_by_ids(&[two.id, Uuid::new_v4(), one.id])
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
            found.sort();
            let mut expected = vec![one.id, two.id];
            expected.sort();
            assert_eq!(found, expected, "an unknown id is skipped");
            assert!(repo.find_by_ids(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn enrichment_replaces_the_runtime_and_rating() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(CreateMovie::new(
                    format!("Timed {}", Uuid::new_v4()),
                    None,
                    Some(::std::time::Duration::from_secs(90 * 60)),
                ))
                .await
                .unwrap();

            repo.apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: movie.title.clone(),
                    runtime_mins: Some(121),
                    rating: Some(7.5),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let stored = repo.find_by_id(movie.id).await.unwrap().unwrap();
            assert_eq!(
                stored.runtime,
                Some(::std::time::Duration::from_secs(121 * 60))
            );
            assert_eq!(stored.rating_tmdb, Some(7.5));
        }

        #[tokio::test]
        async fn delete_orphaned_removes_only_fileless_movies_created_before_the_cutoff() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let bare = repo
                .find_or_create_by_identity(parsed("No entry", None))
                .await
                .unwrap();
            let entry_only = repo
                .find_or_create_by_identity(parsed("Entry without file", None))
                .await
                .unwrap();
            entry_for(&fixture, &entry_only, false).await;
            let kept = repo
                .find_or_create_by_identity(parsed("With file", None))
                .await
                .unwrap();
            entry_for(&fixture, &kept, true).await;
            // A fileless entry beside a present one goes; the movie stays.
            entry_for(&fixture, &kept, false).await;

            assert_eq!(
                repo.delete_orphaned(bare.created_at - ::chrono::Duration::seconds(1))
                    .await
                    .unwrap(),
                0,
                "nothing was created before the cutoff"
            );
            assert!(repo.find_by_id(bare.id).await.unwrap().is_some());

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 2);
            assert!(repo.find_by_id(bare.id).await.unwrap().is_none());
            assert!(repo.find_by_id(entry_only.id).await.unwrap().is_none());
            assert!(repo.find_by_id(kept.id).await.unwrap().is_some());
            assert_eq!(
                repo.find_entries_by_movie_id(kept.id).await.unwrap().len(),
                1,
                "only the entry a file references survives"
            );
        }

        #[tokio::test]
        async fn a_soft_deleted_file_keeps_its_movie_until_it_is_purged() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(parsed("Away", None))
                .await
                .unwrap();
            let file = entry_for(&fixture, &movie, true).await.unwrap();
            fixture
                .files()
                .mark_missing(vec![file.id], ::chrono::Utc::now())
                .await
                .unwrap();

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 0);
            assert!(repo.find_by_id(movie.id).await.unwrap().is_some());

            fixture.files().purge_missing(vec![file.id]).await.unwrap();
            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 1);
            assert!(repo.find_by_id(movie.id).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn an_unkeyed_movie_is_never_matched_and_can_be_keyed_once() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Legacy", Some(1999));
            let legacy = fixture
                .new_unkeyed_movie(&create.title, ::chrono::Utc::now())
                .await;

            assert!(
                repo.find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|m| m.id == legacy)
            );
            assert!(
                repo.assign_identity_key(legacy, &create.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|m| m.id == legacy),
                "a keyed movie is no longer unkeyed"
            );
            assert_eq!(
                repo.find_or_create_by_identity(create).await.unwrap().id,
                legacy,
                "once keyed, the legacy movie is found by its key"
            );
            assert!(
                !repo
                    .assign_identity_key(legacy, "another key|", CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a key is assigned once, never replaced"
            );
        }

        #[tokio::test]
        async fn unkeyed_movies_are_listed_oldest_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            // Whole seconds: Postgres keeps microseconds, the double keeps
            // nanoseconds, and the contract must not depend on either.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let title = format!("Legacy Movie {}", Uuid::new_v4());
            // Inserted newest first, so insertion order is not the answer.
            let newest = fixture
                .new_unkeyed_movie(&title, base + ::chrono::Duration::days(2))
                .await;
            let oldest = fixture.new_unkeyed_movie(&title, base).await;
            let middle = fixture
                .new_unkeyed_movie(&title, base + ::chrono::Duration::days(1))
                .await;

            let listed: Vec<Uuid> = repo
                .find_unkeyed()
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .filter(|id| [newest, oldest, middle].contains(id))
                .collect();
            assert_eq!(
                listed,
                vec![oldest, middle, newest],
                "of legacy duplicates, the original comes first"
            );
        }

        #[tokio::test]
        async fn an_unkeyed_movie_is_not_matched_and_cannot_take_a_held_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Duplicate", Some(2001));
            let legacy = fixture
                .new_unkeyed_movie(&create.title, ::chrono::Utc::now())
                .await;

            let keyed = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();
            assert_ne!(keyed.id, legacy, "a keyless row is never matched");

            assert!(
                !repo
                    .assign_identity_key(legacy, &create.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a key another movie holds is refused"
            );
            assert_eq!(
                repo.find_by_id(legacy).await.unwrap().unwrap().identity_key,
                None
            );
            assert!(
                !repo
                    .assign_identity_key(Uuid::new_v4(), "nobody|", CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "an unknown movie is not keyed"
            );
        }

        #[tokio::test]
        async fn a_movie_key_records_the_rules_version_that_derived_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let current = repo
                .find_or_create_by_identity(new_movie("Current"))
                .await
                .unwrap();
            let stale = repo
                .find_or_create_by_identity(CreateMovie {
                    identity_key_version: 0,
                    ..new_movie("Stale")
                })
                .await
                .unwrap();
            // Whole seconds, as in the oldest-first test above.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let legacy = new_movie("Legacy");
            let older = fixture.new_unkeyed_movie(&legacy.title, base).await;
            assert!(
                repo.assign_identity_key(older, &legacy.identity_key, 0)
                    .await
                    .unwrap()
            );
            let fresh = new_movie("Fresh");
            let fresh_legacy = fixture.new_unkeyed_movie(&fresh.title, base).await;
            assert!(
                repo.assign_identity_key(fresh_legacy, &fresh.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            let unkeyed = fixture
                .new_unkeyed_movie(&format!("Unkeyed {}", Uuid::new_v4()), base)
                .await;
            let ours = [current.id, stale.id, older, fresh_legacy, unkeyed];

            let listed = |version: u16| async move {
                repo.find_keyed_before_version(version)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|t| t.id)
                    .filter(|id| ours.contains(id))
                    .collect::<Vec<Uuid>>()
            };
            assert_eq!(
                listed(CLASSIFIER_VERSION).await,
                vec![older, stale.id],
                "keys older rules derived, oldest first; never a keyless row"
            );
            let mut next = listed(CLASSIFIER_VERSION + 1).await;
            next.sort();
            let mut keyed = vec![current.id, stale.id, older, fresh_legacy];
            keyed.sort();
            assert_eq!(next, keyed, "every key is older than rules not yet written");
        }

        #[tokio::test]
        async fn a_movie_is_found_by_its_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let created = repo
                .find_or_create_by_identity(new_movie("Keyed"))
                .await
                .unwrap();
            let key = created.identity_key.clone().unwrap();

            assert_eq!(
                repo.find_by_identity_key(&key).await.unwrap().map(|t| t.id),
                Some(created.id)
            );
            assert!(
                repo.find_by_identity_key(&format!("nobody {}|", Uuid::new_v4()))
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn rekeying_a_movie_replaces_its_key_and_keeps_the_movie() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let old = CreateMovie {
                identity_key_version: 0,
                ..new_movie("Old Spelling")
            };
            let created = repo.find_or_create_by_identity(old.clone()).await.unwrap();
            repo.apply_enrichment(
                created.id,
                &MovieEnrichment {
                    title: "Provider Title".to_string(),
                    tmdb_id: Some(42),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let new = new_movie("New Spelling");

            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap()
            );
            let stored = repo
                .find_by_identity_key(&new.identity_key)
                .await
                .unwrap()
                .expect("found by its new key");
            assert_eq!(stored.id, created.id, "the same row, not a new one");
            assert_eq!(stored.tmdb_id, Some(42), "with its enrichment");
            assert!(
                repo.find_by_identity_key(&old.identity_key)
                    .await
                    .unwrap()
                    .is_none(),
                "the old key is free"
            );
            assert!(
                !repo
                    .find_keyed_before_version(CLASSIFIER_VERSION)
                    .await
                    .unwrap()
                    .iter()
                    .any(|t| t.id == created.id),
                "the new key carries the version that derived it"
            );
            assert_eq!(
                repo.find_or_create_by_identity(new.clone())
                    .await
                    .unwrap()
                    .id,
                created.id,
                "the next file of the new spelling finds it"
            );
            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap(),
                "a key it already holds is no clash"
            );
        }

        #[tokio::test]
        async fn rekeying_a_movie_refuses_a_held_key_and_can_release_its_own() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let a = repo
                .find_or_create_by_identity(new_movie("A"))
                .await
                .unwrap();
            let b = repo
                .find_or_create_by_identity(new_movie("B"))
                .await
                .unwrap();
            let b_key = b.identity_key.clone().unwrap();

            assert!(
                !repo
                    .rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "another title holds it"
            );
            assert_eq!(
                repo.find_by_id(a.id).await.unwrap().unwrap().identity_key,
                a.identity_key,
                "a refused rekey changes nothing"
            );

            assert!(repo.rekey(b.id, None, CLASSIFIER_VERSION).await.unwrap());
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().identity_key,
                None,
                "released: never matched again"
            );
            assert!(
                repo.rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a released key is free"
            );
            assert!(
                !repo
                    .rekey(
                        Uuid::new_v4(),
                        Some("nobody|".to_string()),
                        CLASSIFIER_VERSION
                    )
                    .await
                    .unwrap(),
                "an unknown title is not rekeyed"
            );
        }
    };
}

/// Behavioural contract for [`crate::repositories::LibraryShapeRepository`].
///
/// `$setup` names an `async fn() -> impl LibraryShapeFixture` whose store is
/// empty.
#[macro_export]
macro_rules! library_shape_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
        use $crate::models::library::CreateLibrary;
        use $crate::models::library_shape::{FilesByContentType, LibraryShape, NamedCount};
        use $crate::models::movie::{CreateMovie, CreateMovieEntry};
        use $crate::models::show::{CreateEpisode, CreateShow};
        use $crate::models::stream::{
            AudioStreamMetadata, CreateMediaStream, StreamMetadata, StreamType,
            SubtitleStreamMetadata, VideoStreamMetadata,
        };
        use $crate::repositories::contract::fixture::LibraryShapeFixture;
        use $crate::utils::telemetry::{FileSizeBucket, GIB, UNKNOWN_LABEL};

        async fn new_library(fixture: &impl LibraryShapeFixture) -> Uuid {
            let unique = Uuid::new_v4();
            fixture
                .libraries()
                .create(CreateLibrary {
                    name: format!("contract library {unique}"),
                    root_path: ::std::path::PathBuf::from(format!("/media/{unique}")),
                    description: None,
                })
                .await
                .expect("create a library")
                .id
        }

        /// A movie entry, in `library_id`, of a movie of its own.
        async fn new_movie_entry(fixture: &impl LibraryShapeFixture, library_id: Uuid) -> Uuid {
            let movie = fixture
                .movies()
                .find_or_create_by_identity(CreateMovie::new(
                    format!("contract movie {}", Uuid::new_v4()),
                    None,
                    None,
                ))
                .await
                .expect("create a movie");
            fixture
                .movies()
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                    is_primary: true,
                })
                .await
                .expect("create a movie entry")
                .id
        }

        /// Episodes numbered `numbers` in season 1 of a show of their own.
        async fn new_episodes(fixture: &impl LibraryShapeFixture, numbers: &[u32]) -> Vec<Uuid> {
            let show = fixture
                .shows()
                .find_or_create_by_identity(CreateShow::new(
                    format!("contract show {}", Uuid::new_v4()),
                    None,
                ))
                .await
                .expect("create a show");
            let season = fixture
                .shows()
                .find_or_create_season(show.id, 1)
                .await
                .expect("create a season");
            let mut ids = Vec::new();
            for number in numbers {
                let episode = fixture
                    .shows()
                    .find_or_create_episode(CreateEpisode {
                        season_id: season.id,
                        episode_number: *number,
                        title: format!("Episode {number}"),
                        runtime: None,
                        air_date: None,
                    })
                    .await
                    .expect("create an episode");
                ids.push(episode.id);
            }
            ids
        }

        fn movie(movie_entry_id: Uuid) -> Option<MediaFileContent> {
            Some(MediaFileContent::Movie { movie_entry_id })
        }

        fn episode(episode_id: Uuid) -> Option<MediaFileContent> {
            Some(MediaFileContent::episode(episode_id))
        }

        /// A present file of `size_bytes` in `library_id`. An unclassified
        /// file is `Unknown`, as the schema's check constraint requires.
        async fn new_file(
            fixture: &impl LibraryShapeFixture,
            library_id: Uuid,
            content: Option<MediaFileContent>,
            container: Option<&str>,
            size_bytes: u64,
        ) -> Uuid {
            let unique = Uuid::new_v4();
            let status = if content.is_some() {
                FileStatus::Known
            } else {
                FileStatus::Unknown
            };
            fixture
                .files()
                .create(CreateMediaFile {
                    library_id,
                    path: ::std::path::PathBuf::from(format!("/media/{library_id}/{unique}.mkv")),
                    // Positive and unique: the hash is a signed BIGINT column.
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes,
                    mtime: None,
                    mime_type: None,
                    duration: None,
                    container_format: container.map(str::to_string),
                    content,
                    status,
                    classifier_version: 0,
                })
                .await
                .expect("create a file")
                .id
        }

        async fn mark_missing(fixture: &impl LibraryShapeFixture, file_id: Uuid) {
            let marked = fixture
                .files()
                .mark_missing(vec![file_id], ::chrono::Utc::now())
                .await
                .expect("mark a file missing");
            assert_eq!(marked, 1);
        }

        fn stream(
            file_id: Uuid,
            index: u32,
            stream_type: StreamType,
            codec: &str,
        ) -> CreateMediaStream {
            let metadata = match stream_type {
                StreamType::Video => StreamMetadata::Video(VideoStreamMetadata {
                    width: 1920,
                    height: 1080,
                    frame_rate: None,
                    bit_rate: None,
                    color_space: None,
                    color_range: None,
                    hdr_format: None,
                }),
                StreamType::Audio => StreamMetadata::Audio(AudioStreamMetadata {
                    language: None,
                    title: None,
                    channels: 2,
                    sample_rate: 48_000,
                    channel_layout: None,
                    bit_rate: None,
                    is_default: false,
                    is_forced: false,
                }),
                StreamType::Subtitle => StreamMetadata::Subtitle(SubtitleStreamMetadata {
                    language: None,
                    title: None,
                    is_default: false,
                    is_forced: false,
                }),
            };
            CreateMediaStream {
                file_id,
                index,
                stream_type,
                codec: codec.to_string(),
                metadata,
            }
        }

        #[tokio::test]
        async fn an_empty_store_has_an_empty_shape() {
            let fixture = $setup().await;

            assert_eq!(
                fixture.repo().shape().await.unwrap(),
                LibraryShape::default()
            );
        }

        #[tokio::test]
        async fn an_empty_library_counts_as_a_library_and_as_nothing_else() {
            let fixture = $setup().await;
            new_library(&fixture).await;
            new_library(&fixture).await;

            assert_eq!(
                fixture.repo().shape().await.unwrap(),
                LibraryShape {
                    libraries: 2,
                    ..LibraryShape::default()
                }
            );
        }

        #[tokio::test]
        async fn files_are_counted_by_content_type_and_container() {
            let fixture = $setup().await;
            let library = new_library(&fixture).await;
            let entry = new_movie_entry(&fixture, library).await;
            let episodes = new_episodes(&fixture, &[1]).await;
            new_file(&fixture, library, movie(entry), Some("matroska,webm"), 10).await;
            new_file(
                &fixture,
                library,
                movie(entry),
                Some("mov,mp4,m4a,3gp,3g2,mj2"),
                10,
            )
            .await;
            new_file(
                &fixture,
                library,
                episode(episodes[0]),
                Some("matroska,webm"),
                10,
            )
            .await;
            new_file(&fixture, library, None, None, 10).await;

            let shape = fixture.repo().shape().await.unwrap();

            assert_eq!(
                shape.files,
                FilesByContentType {
                    movie: 2,
                    episode: 1,
                    unclassified: 1,
                }
            );
            assert_eq!(
                shape.containers,
                vec![
                    NamedCount::new("matroska,webm", 2),
                    NamedCount::new("mov,mp4,m4a,3gp,3g2,mj2", 1),
                    NamedCount::new(UNKNOWN_LABEL, 1),
                ],
                "sorted by name, and a file with no container is `unknown`"
            );
            assert_eq!(shape.movies, 1, "two files of one movie are one movie");
            assert_eq!((shape.shows, shape.seasons, shape.episodes), (1, 1, 1));
            assert_eq!(shape.total_bytes, 40);
        }

        /// A multi-episode file (`S01E01E02`, issue #182) is attached to its
        /// first episode and records the rest of its range on the file, not as
        /// episode rows. The report counts what a user could browse: one
        /// episode file and one episode, not one per number in the range.
        #[tokio::test]
        async fn a_multi_episode_file_counts_as_one_file_of_one_episode() {
            let fixture = $setup().await;
            let library = new_library(&fixture).await;
            let episodes = new_episodes(&fixture, &[1]).await;
            new_file(
                &fixture,
                library,
                Some(MediaFileContent::Episode {
                    episode_id: episodes[0],
                    last_episode_number: Some(2),
                }),
                Some("matroska,webm"),
                10,
            )
            .await;

            let shape = fixture.repo().shape().await.unwrap();

            assert_eq!(
                shape.files,
                FilesByContentType {
                    movie: 0,
                    episode: 1,
                    unclassified: 0,
                }
            );
            assert_eq!((shape.shows, shape.seasons, shape.episodes), (1, 1, 1));
        }

        #[tokio::test]
        async fn a_missing_file_and_its_streams_are_not_counted() {
            let fixture = $setup().await;
            let library = new_library(&fixture).await;
            let kept_entry = new_movie_entry(&fixture, library).await;
            let gone_entry = new_movie_entry(&fixture, library).await;
            let episodes = new_episodes(&fixture, &[1, 2]).await;
            let kept = new_file(
                &fixture,
                library,
                movie(kept_entry),
                Some("matroska,webm"),
                GIB,
            )
            .await;
            let gone = new_file(&fixture, library, movie(gone_entry), Some("avi"), 7 * GIB).await;
            new_file(
                &fixture,
                library,
                episode(episodes[0]),
                Some("matroska,webm"),
                1,
            )
            .await;
            let gone_episode = new_file(
                &fixture,
                library,
                episode(episodes[1]),
                Some("matroska,webm"),
                1,
            )
            .await;
            fixture
                .streams()
                .insert_streams(vec![
                    stream(kept, 0, StreamType::Video, "h264"),
                    stream(gone, 0, StreamType::Video, "hevc"),
                    stream(gone, 1, StreamType::Audio, "dts"),
                ])
                .await
                .unwrap();
            mark_missing(&fixture, gone).await;
            mark_missing(&fixture, gone_episode).await;

            let shape = fixture.repo().shape().await.unwrap();

            assert_eq!(
                shape.files,
                FilesByContentType {
                    movie: 1,
                    episode: 1,
                    unclassified: 0,
                },
                "the missing files are not counted"
            );
            assert_eq!(
                shape.movies, 1,
                "a movie whose only file is missing is not live"
            );
            assert_eq!(
                (shape.shows, shape.seasons, shape.episodes),
                (1, 1, 1),
                "an episode whose only file is missing is not counted"
            );
            assert_eq!(shape.containers, vec![NamedCount::new("matroska,webm", 2)]);
            assert_eq!(shape.video_codecs, vec![NamedCount::new("h264", 1)]);
            assert!(
                shape.audio_codecs.is_empty(),
                "a missing file's streams are not counted"
            );
            assert_eq!(shape.total_bytes, GIB + 1);
            assert_eq!(shape.file_sizes.count(FileSizeBucket::From4To10Gib), 0);
        }

        #[tokio::test]
        async fn streams_are_counted_per_type_and_codec() {
            let fixture = $setup().await;
            let library = new_library(&fixture).await;
            let first = new_file(&fixture, library, None, Some("matroska,webm"), 1).await;
            let second = new_file(&fixture, library, None, Some("matroska,webm"), 1).await;
            fixture
                .streams()
                .insert_streams(vec![
                    stream(first, 0, StreamType::Video, "hevc"),
                    stream(first, 1, StreamType::Audio, "eac3"),
                    stream(first, 2, StreamType::Audio, "aac"),
                    stream(first, 3, StreamType::Subtitle, "subrip"),
                    stream(second, 0, StreamType::Video, "h264"),
                    stream(second, 1, StreamType::Audio, "aac"),
                ])
                .await
                .unwrap();

            let shape = fixture.repo().shape().await.unwrap();

            assert_eq!(
                shape.video_codecs,
                vec![NamedCount::new("h264", 1), NamedCount::new("hevc", 1)]
            );
            assert_eq!(
                shape.audio_codecs,
                vec![NamedCount::new("aac", 2), NamedCount::new("eac3", 1)]
            );
            assert_eq!(shape.subtitle_codecs, vec![NamedCount::new("subrip", 1)]);
        }

        #[tokio::test]
        async fn file_sizes_are_bucketed_at_each_boundary() {
            let fixture = $setup().await;
            let library = new_library(&fixture).await;
            let mut expected_total = 0u64;
            // Each boundary and one byte short of it: the pair straddles the
            // edge, so a `<` written as `<=` on either side moves a file.
            for bucket in &FileSizeBucket::ALL[1..] {
                let bound = bucket.lower_bound_bytes();
                for size in [bound - 1, bound] {
                    new_file(&fixture, library, None, None, size).await;
                    expected_total += size;
                }
            }

            let shape = fixture.repo().shape().await.unwrap();

            let last = FileSizeBucket::ALL.len() - 1;
            for (bucket, count) in shape.file_sizes.iter() {
                // The first and last buckets each hold one side of one edge;
                // every other bucket holds one side of two.
                let edges =
                    if bucket == FileSizeBucket::ALL[0] || bucket == FileSizeBucket::ALL[last] {
                        1
                    } else {
                        2
                    };
                assert_eq!(count, edges, "{bucket:?}");
            }
            assert_eq!(shape.file_sizes.total(), shape.files.total());
            assert_eq!(shape.total_bytes, expected_total);
        }
    };
}

/// Behavioural contract for [`crate::repositories::PlaybackTelemetryRepository`]
/// (issue #143).
///
/// `$setup` names an `async fn() -> impl PlaybackTelemetryFixture` whose
/// repository starts empty.
#[macro_export]
macro_rules! playback_telemetry_repository_contract {
    ($setup:path) => {
        use ::chrono::NaiveDate;
        use $crate::models::playback_telemetry::test_utils::{rebuffer_key, start_key, switch_key};
        use $crate::models::playback_telemetry::{
            BitrateClass, ClientKind, FailureReason, FailureStage, HeightClass,
            PlaybackTelemetryEvent, PlaybackTelemetrySummary, RebufferBucket, RebufferKey,
            StartKey, StartOutcome, SwitchKey, SwitchTrigger,
        };
        use $crate::repositories::contract::fixture::PlaybackTelemetryFixture;

        fn day(n: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 9, n).expect("a September day")
        }

        fn start_event(key: StartKey) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Start(key)
        }

        fn rebuffer_event(key: RebufferKey, duration_ms: u32) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Rebuffer { key, duration_ms }
        }

        fn switch_event(key: SwitchKey) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Switch(key)
        }

        #[tokio::test]
        async fn a_start_recorded_twice_counts_two() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            assert_eq!(summary.starts.len(), 1);
            assert_eq!(summary.starts[0].key, start_key());
            assert_eq!(summary.starts[0].count, 2);
        }

        /// One batch mixing every kind, naming some keys more than once,
        /// reads back as its own tally -- every event counted once, repeats
        /// folded into one row -- and a second batch adds to what the first
        /// counted.
        #[tokio::test]
        async fn a_batch_counts_each_event_once_and_adds_to_what_is_kept() {
            let fixture = $setup().await;
            let hd = StartKey {
                height_class: HeightClass::Hd,
                ..start_key()
            };
            let batch = vec![
                PlaybackTelemetryEvent::Start(start_key()),
                PlaybackTelemetryEvent::Switch(switch_key()),
                PlaybackTelemetryEvent::Rebuffer {
                    key: rebuffer_key(),
                    duration_ms: 500,
                },
                PlaybackTelemetryEvent::Start(hd.clone()),
                PlaybackTelemetryEvent::Start(start_key()),
                PlaybackTelemetryEvent::Rebuffer {
                    key: rebuffer_key(),
                    duration_ms: 12_000,
                },
            ];

            fixture.repo().record_batch(day(1), &batch).await.unwrap();

            let once = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(once, PlaybackTelemetrySummary::tally(&batch));

            fixture.repo().record_batch(day(1), &batch).await.unwrap();

            let twice = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            let doubled: Vec<PlaybackTelemetryEvent> =
                batch.iter().chain(batch.iter()).cloned().collect();
            assert_eq!(twice, PlaybackTelemetrySummary::tally(&doubled));
        }

        #[tokio::test]
        async fn an_empty_batch_counts_nothing() {
            let fixture = $setup().await;

            fixture.repo().record_batch(day(1), &[]).await.unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(summary, Default::default());
        }

        /// Every dimension is part of the key: a start differing in any one
        /// of them is counted apart, and the list comes back sorted by key.
        #[tokio::test]
        async fn every_dimension_separates_starts() {
            let fixture = $setup().await;
            let variants = vec![
                start_key(),
                StartKey {
                    client_kind: ClientKind::Android,
                    ..start_key()
                },
                StartKey {
                    outcome: StartOutcome::Failed {
                        reason: FailureReason::VideoCodec,
                        stage: FailureStage::Preflight,
                    },
                    ..start_key()
                },
                StartKey {
                    outcome: StartOutcome::Failed {
                        reason: FailureReason::VideoCodec,
                        stage: FailureStage::Playback,
                    },
                    ..start_key()
                },
                StartKey {
                    container: "mov,mp4,m4a,3gp,3g2,mj2".to_string(),
                    ..start_key()
                },
                StartKey {
                    video_codec: "hevc".to_string(),
                    ..start_key()
                },
                StartKey {
                    audio_codec: "eac3".to_string(),
                    ..start_key()
                },
                StartKey {
                    height_class: HeightClass::Uhd,
                    ..start_key()
                },
            ];
            for key in variants.iter().rev() {
                fixture
                    .repo()
                    .record_batch(day(1), &[start_event(key.clone())])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let mut expected = variants.clone();
            expected.sort();
            let keys: Vec<StartKey> = summary.starts.iter().map(|s| s.key.clone()).collect();
            assert_eq!(keys, expected);
            assert!(summary.starts.iter().all(|s| s.count == 1));
        }

        /// Every value of every vocabulary survives the round trip through
        /// storage, so no label is written one way and read another.
        #[tokio::test]
        async fn every_vocabulary_value_round_trips() {
            let fixture = $setup().await;
            let mut starts = Vec::new();
            for client_kind in ClientKind::ALL {
                starts.push(StartKey {
                    client_kind: *client_kind,
                    ..start_key()
                });
            }
            for reason in FailureReason::ALL {
                for stage in FailureStage::ALL {
                    starts.push(StartKey {
                        outcome: StartOutcome::Failed {
                            reason: *reason,
                            stage: *stage,
                        },
                        ..start_key()
                    });
                }
            }
            for height_class in HeightClass::ALL {
                starts.push(StartKey {
                    height_class: *height_class,
                    ..start_key()
                });
            }
            starts.sort();
            starts.dedup();
            for key in &starts {
                fixture
                    .repo()
                    .record_batch(day(1), &[start_event(key.clone())])
                    .await
                    .unwrap();
            }
            let mut rebuffers = Vec::new();
            for bitrate_class in BitrateClass::ALL {
                rebuffers.push(RebufferKey {
                    bitrate_class: *bitrate_class,
                    ..rebuffer_key()
                });
            }
            for key in &rebuffers {
                fixture
                    .repo()
                    .record_batch(day(1), &[rebuffer_event(key.clone(), 10)])
                    .await
                    .unwrap();
            }
            let mut switches = Vec::new();
            for trigger in SwitchTrigger::ALL {
                for to_height_class in HeightClass::ALL {
                    switches.push(SwitchKey {
                        trigger: *trigger,
                        to_height_class: *to_height_class,
                        ..switch_key()
                    });
                }
            }
            for key in &switches {
                fixture
                    .repo()
                    .record_batch(day(1), &[switch_event(*key)])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let read: Vec<StartKey> = summary.starts.iter().map(|s| s.key.clone()).collect();
            assert_eq!(read, starts);
            let read: Vec<RebufferKey> = summary.rebuffers.iter().map(|r| r.key.clone()).collect();
            rebuffers.sort();
            assert_eq!(read, rebuffers);
            let read: Vec<SwitchKey> = summary.switches.iter().map(|s| s.key).collect();
            switches.sort();
            assert_eq!(read, switches);
        }

        /// A counter is kept per day, and a summary sums the days it spans.
        #[tokio::test]
        async fn counters_are_kept_per_day_and_summed_across_a_range() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(2), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(2), &[start_event(start_key())])
                .await
                .unwrap();

            let first = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            let second = fixture.repo().summarize(day(2), day(2)).await.unwrap();
            let both = fixture.repo().summarize(day(1), day(2)).await.unwrap();

            assert_eq!(first.starts[0].count, 1);
            assert_eq!(second.starts[0].count, 2);
            assert_eq!(both.starts.len(), 1, "one entry per key, not per day");
            assert_eq!(both.starts[0].count, 3);
        }

        /// Both ends of the range are included; a day either side is not.
        #[tokio::test]
        async fn a_summary_includes_both_ends_and_nothing_outside() {
            let fixture = $setup().await;
            for n in [1, 2, 4, 5] {
                fixture
                    .repo()
                    .record_batch(day(n), &[start_event(start_key())])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[rebuffer_event(rebuffer_key(), 100)])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[switch_event(switch_key())])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(2), day(4)).await.unwrap();

            assert_eq!(summary.starts[0].count, 2);
            assert_eq!(summary.rebuffers[0].events, 2);
            assert_eq!(summary.switches[0].count, 2);
            let gap = fixture.repo().summarize(day(3), day(3)).await.unwrap();
            assert_eq!(gap, Default::default());
            let backwards = fixture.repo().summarize(day(4), day(2)).await.unwrap();
            assert_eq!(backwards, Default::default());
        }

        /// A rebuffer adds one event, its duration to the total, and one to
        /// the range it falls into -- straddling a boundary lands on either
        /// side of it.
        #[tokio::test]
        async fn a_rebuffer_adds_its_duration_and_its_bucket() {
            let fixture = $setup().await;
            for duration_ms in [999, 1_000, 30_000] {
                fixture
                    .repo()
                    .record_batch(day(1), &[rebuffer_event(rebuffer_key(), duration_ms)])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let row = &summary.rebuffers[0];
            assert_eq!(row.key, rebuffer_key());
            assert_eq!(row.events, 3);
            assert_eq!(row.total_ms, 31_999);
            assert_eq!(row.histogram.count(RebufferBucket::Under1Secs), 1);
            assert_eq!(row.histogram.count(RebufferBucket::From1To3Secs), 1);
            assert_eq!(row.histogram.count(RebufferBucket::From3To10Secs), 0);
            assert_eq!(row.histogram.count(RebufferBucket::From10To30Secs), 0);
            assert_eq!(row.histogram.count(RebufferBucket::AtLeast30Secs), 1);
        }

        #[tokio::test]
        async fn a_switch_recorded_twice_counts_two() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(switch_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(switch_key())])
                .await
                .unwrap();
            let auto = SwitchKey {
                trigger: SwitchTrigger::Auto,
                ..switch_key()
            };
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(auto)])
                .await
                .unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let counts: Vec<(SwitchKey, u64)> =
                summary.switches.iter().map(|s| (s.key, s.count)).collect();
            let mut expected = vec![(switch_key(), 2), (auto, 1)];
            expected.sort();
            assert_eq!(counts, expected);
        }

        /// Pruning removes the days strictly before the cutoff, in every
        /// table, and counts the day-rows it removed.
        #[tokio::test]
        async fn pruning_removes_only_days_before_the_cutoff() {
            let fixture = $setup().await;
            for n in [1, 2, 3] {
                fixture
                    .repo()
                    .record_batch(day(n), &[start_event(start_key())])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[rebuffer_event(rebuffer_key(), 100)])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[switch_event(switch_key())])
                    .await
                    .unwrap();
            }

            let removed = fixture.repo().prune_before(day(2)).await.unwrap();

            assert_eq!(removed, 3, "one row per table for the one day before");
            let before = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(before, Default::default());
            let kept = fixture.repo().summarize(day(2), day(3)).await.unwrap();
            assert_eq!(kept.starts[0].count, 2);
            assert_eq!(kept.rebuffers[0].events, 2);
            assert_eq!(kept.switches[0].count, 2);
            assert_eq!(fixture.repo().prune_before(day(2)).await.unwrap(), 0);
        }
    };
}

/// Behavioural contract for [`crate::repositories::CatalogRepository`]: one
/// listing of movies and shows together, filtered, ordered by every sort field
/// in both directions with missing values last, paged from a position either
/// way without a title repeated or skipped -- and never listing a title with
/// no present file (issues #179, #183, #187).
///
/// Titles are single ASCII words: the SQL catalogue orders by `lower(title)`
/// under the database's collation, which agrees with the in-memory byte order
/// only there.
///
/// `$setup` names an `async fn() -> impl CatalogRepositoryFixture`.
#[macro_export]
macro_rules! catalog_repository_contract {
    ($setup:path) => {
        use ::std::collections::HashMap;
        use ::std::num::NonZeroU32;
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::models::catalog::{
            CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
            SortDirection, SortKey, TitleKind,
        };
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
        use $crate::models::movie::{CreateMovie, CreateMovieEntry};
        use $crate::models::show::{CreateEpisode, CreateShow};
        use $crate::providers::enrichment::{MovieEnrichment, ShowEnrichment};
        use $crate::repositories::contract::fixture::CatalogRepositoryFixture;

        /// A title the contract made: its id and its file's, when it has one.
        #[derive(Debug, Clone, Copy)]
        struct Made {
            id: Uuid,
            file: Option<Uuid>,
        }

        /// A present file in the library `library_id`.
        async fn present_file(
            fixture: &impl CatalogRepositoryFixture,
            library_id: Uuid,
            content: MediaFileContent,
        ) -> Uuid {
            let unique = Uuid::new_v4();
            fixture
                .files()
                .create(CreateMediaFile {
                    library_id,
                    path: ::std::path::PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .expect("create a file")
                .id
        }

        /// A movie as the indexer and enrichment leave it, with a present file
        /// when `with_file`.
        async fn movie(
            fixture: &impl CatalogRepositoryFixture,
            name: &str,
            year: Option<u32>,
            runtime_mins: Option<u32>,
            rating: Option<f32>,
            with_file: bool,
        ) -> Made {
            let movies = fixture.movies();
            let movie = movies
                .find_or_create_by_identity(CreateMovie::new(
                    name,
                    year,
                    runtime_mins.map(|mins| Duration::from_secs(u64::from(mins) * 60)),
                ))
                .await
                .expect("create a movie");
            if rating.is_some() {
                movies
                    .apply_enrichment(
                        movie.id,
                        &MovieEnrichment {
                            title: name.to_string(),
                            year,
                            runtime_mins,
                            rating,
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("rate the movie");
            }
            let library_id = fixture.new_library().await;
            let entry = movies
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                    is_primary: true,
                })
                .await
                .expect("create an entry");
            let file = if with_file {
                Some(
                    present_file(
                        fixture,
                        library_id,
                        MediaFileContent::Movie {
                            movie_entry_id: entry.id,
                        },
                    )
                    .await,
                )
            } else {
                None
            };
            Made { id: movie.id, file }
        }

        /// A show with one season of `episodes` episodes, each with a present
        /// file when `with_files`. Returns the show and its episodes' files.
        async fn show(
            fixture: &impl CatalogRepositoryFixture,
            name: &str,
            year: Option<u32>,
            rating: Option<f32>,
            episodes: u32,
            with_files: bool,
        ) -> (Uuid, Vec<Uuid>) {
            let shows = fixture.shows();
            let show = shows
                .find_or_create_by_identity(CreateShow::new(name, year))
                .await
                .expect("create a show");
            if rating.is_some() {
                shows
                    .apply_enrichment(
                        show.id,
                        &ShowEnrichment {
                            title: name.to_string(),
                            year,
                            rating,
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("rate the show");
            }
            let season = shows
                .find_or_create_season(show.id, 1)
                .await
                .expect("create a season");
            let library_id = fixture.new_library().await;
            let mut files = Vec::new();
            for number in 1..=episodes {
                let episode = shows
                    .find_or_create_episode(CreateEpisode {
                        season_id: season.id,
                        episode_number: number,
                        title: format!("Episode {number}"),
                        runtime: None,
                        air_date: None,
                    })
                    .await
                    .expect("create an episode");
                if with_files {
                    files.push(
                        present_file(fixture, library_id, MediaFileContent::episode(episode.id))
                            .await,
                    );
                }
            }
            (show.id, files)
        }

        /// Seven live titles covering every sort field's interesting cases:
        /// both kinds, mixed-case titles, tied and missing years and ratings,
        /// and runtimes only films have. Returns each title's id and name, in
        /// the order they were created.
        async fn seed(fixture: &impl CatalogRepositoryFixture) -> Vec<(Uuid, &'static str)> {
            vec![
                (movie(fixture, "alpha", Some(2001), Some(100), Some(8.0), true).await.id, "alpha"),
                (movie(fixture, "Bravo", None, None, None, true).await.id, "Bravo"),
                (movie(fixture, "charlie", Some(1999), Some(90), None, true).await.id, "charlie"),
                (show(fixture, "delta", Some(2010), Some(9.0), 1, true).await.0, "delta"),
                (movie(fixture, "Echo", Some(2001), None, Some(6.0), true).await.id, "Echo"),
                (show(fixture, "Foxtrot", None, None, 1, true).await.0, "Foxtrot"),
                (show(fixture, "golf", Some(1999), Some(6.0), 1, true).await.0, "golf"),
            ]
        }

        fn every_sort() -> Vec<CatalogSort> {
            let mut sorts = Vec::new();
            for field in [
                CatalogSortField::Title,
                CatalogSortField::Year,
                CatalogSortField::Rating,
                CatalogSortField::DateAdded,
                CatalogSortField::Runtime,
            ] {
                for direction in [SortDirection::Asc, SortDirection::Desc] {
                    sorts.push(CatalogSort { field, direction });
                }
            }
            sorts
        }

        fn sorted(field: CatalogSortField, direction: SortDirection) -> CatalogSort {
            CatalogSort { field, direction }
        }

        fn by_title() -> CatalogSort {
            sorted(CatalogSortField::Title, SortDirection::Asc)
        }

        async fn browse(
            fixture: &impl CatalogRepositoryFixture,
            filters: CatalogFilters,
            sort: CatalogSort,
            seek: Seek,
            limit: u32,
        ) -> Vec<CatalogPosition> {
            fixture
                .repo()
                .browse(&CatalogQuery {
                    filters,
                    sort,
                    seek,
                    limit: NonZeroU32::new(limit).expect("a positive limit"),
                })
                .await
                .expect("browse")
        }

        /// The whole listing in one page.
        async fn everything(
            fixture: &impl CatalogRepositoryFixture,
            filters: CatalogFilters,
            sort: CatalogSort,
        ) -> Vec<CatalogPosition> {
            browse(fixture, filters, sort, Seek::Forward(None), 1000).await
        }

        fn names(rows: &[CatalogPosition], made: &[(Uuid, &'static str)]) -> Vec<&'static str> {
            let by_id: HashMap<Uuid, &'static str> = made.iter().copied().collect();
            rows.iter()
                .map(|row| *by_id.get(&row.id).expect("a title the contract made"))
                .collect()
        }

        /// `rows` lists `groups` in order; within a group, in any order.
        #[track_caller]
        fn assert_grouped(rows: &[&'static str], groups: &[&[&'static str]]) {
            let mut at = 0;
            for group in groups {
                let mut got: Vec<&str> = rows[at..at + group.len()].to_vec();
                let mut want: Vec<&str> = group.to_vec();
                got.sort_unstable();
                want.sort_unstable();
                assert_eq!(got, want, "at {at} of {rows:?}");
                at += group.len();
            }
            assert_eq!(at, rows.len(), "{rows:?} has more than {groups:?}");
        }

        /// The key a title really has for `field`, read back through its own
        /// repository.
        async fn stored_key(
            fixture: &impl CatalogRepositoryFixture,
            row: &CatalogPosition,
            field: CatalogSortField,
        ) -> SortKey {
            match row.kind {
                TitleKind::Movie => {
                    let movie = fixture.movies().find_by_id(row.id).await.unwrap().unwrap();
                    match field {
                        CatalogSortField::Title => SortKey::Title(movie.title.to_lowercase()),
                        CatalogSortField::Year => SortKey::Year(movie.year.map(|y| y as i32)),
                        CatalogSortField::Rating => SortKey::Rating(movie.rating_tmdb),
                        CatalogSortField::DateAdded => SortKey::DateAdded(movie.created_at),
                        CatalogSortField::Runtime => {
                            SortKey::Runtime(movie.runtime.map(|d| (d.as_secs() / 60) as i32))
                        }
                    }
                }
                TitleKind::Show => {
                    let show = fixture.shows().find_by_id(row.id).await.unwrap().unwrap();
                    match field {
                        CatalogSortField::Title => SortKey::Title(show.title.to_lowercase()),
                        CatalogSortField::Year => SortKey::Year(show.year.map(|y| y as i32)),
                        CatalogSortField::Rating => SortKey::Rating(show.rating_tmdb),
                        CatalogSortField::DateAdded => SortKey::DateAdded(show.created_at),
                        CatalogSortField::Runtime => SortKey::Runtime(None),
                    }
                }
            }
        }

        async fn order_of(
            fixture: &impl CatalogRepositoryFixture,
            made: &[(Uuid, &'static str)],
            field: CatalogSortField,
            direction: SortDirection,
        ) -> Vec<&'static str> {
            names(
                &everything(fixture, CatalogFilters::default(), sorted(field, direction)).await,
                made,
            )
        }

        async fn listed_names(
            fixture: &impl CatalogRepositoryFixture,
            made: &[(Uuid, &'static str)],
            filters: CatalogFilters,
        ) -> Vec<&'static str> {
            names(&everything(fixture, filters, by_title()).await, made)
        }

        #[tokio::test]
        async fn titles_of_both_kinds_interleave_by_title_ignoring_case() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;

            let asc = everything(&fixture, CatalogFilters::default(), by_title()).await;
            assert_eq!(
                names(&asc, &made),
                ["alpha", "Bravo", "charlie", "delta", "Echo", "Foxtrot", "golf"]
            );
            assert_eq!(asc[3].kind, TitleKind::Show);
            assert_eq!(asc[3].key, SortKey::Title("delta".to_string()));

            assert_eq!(
                order_of(&fixture, &made, CatalogSortField::Title, SortDirection::Desc).await,
                ["golf", "Foxtrot", "Echo", "delta", "charlie", "Bravo", "alpha"]
            );
        }

        #[tokio::test]
        async fn every_field_sorts_both_ways_with_missing_values_last() {
            use CatalogSortField::{DateAdded, Rating, Runtime, Year};
            use SortDirection::{Asc, Desc};
            let fixture = $setup().await;
            let made = seed(&fixture).await;

            assert_grouped(
                &order_of(&fixture, &made, Year, Asc).await,
                &[&["charlie"], &["golf"], &["alpha", "Echo"], &["delta"], &["Bravo"], &["Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Year, Desc).await,
                &[&["delta"], &["alpha", "Echo"], &["golf"], &["charlie"], &["Foxtrot"], &["Bravo"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Asc).await,
                &[&["Echo"], &["golf"], &["alpha"], &["delta"], &["Bravo", "charlie"], &["Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Desc).await,
                &[&["delta"], &["alpha"], &["golf"], &["Echo"], &["Foxtrot"], &["Bravo", "charlie"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Asc).await,
                &[&["charlie"], &["alpha"], &["Bravo", "Echo"], &["delta", "Foxtrot", "golf"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Desc).await,
                &[&["alpha"], &["charlie"], &["delta", "Foxtrot", "golf"], &["Bravo", "Echo"]],
            );
            let created: Vec<&str> = made.iter().map(|(_, name)| *name).collect();
            assert_eq!(order_of(&fixture, &made, DateAdded, Asc).await, created);
            let mut newest_first = created.clone();
            newest_first.reverse();
            assert_eq!(order_of(&fixture, &made, DateAdded, Desc).await, newest_first);
        }

        /// Whatever the sort, every row carries the key its title really has,
        /// and each row sorts strictly after the one before it -- ties
        /// included, which only the `(kind, id)` tie-break can separate.
        #[tokio::test]
        async fn every_row_carries_its_own_key_in_strict_display_order() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let rows = everything(&fixture, CatalogFilters::default(), sort).await;
                assert_eq!(rows.len(), 7, "{sort:?}");
                for row in &rows {
                    assert_eq!(row.key, stored_key(&fixture, row, sort.field).await, "{sort:?}");
                }
                for pair in rows.windows(2) {
                    assert!(
                        pair[0].display_cmp(&pair[1], sort.direction).is_lt(),
                        "{sort:?}: {:?} before {:?}",
                        pair[0],
                        pair[1]
                    );
                }
            }
        }

        #[tokio::test]
        async fn paging_forward_in_any_page_size_visits_every_title_once() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let whole = everything(&fixture, CatalogFilters::default(), sort).await;
                for size in 1..=3_u32 {
                    let mut walked = Vec::new();
                    let mut after = None;
                    loop {
                        let page = browse(
                            &fixture,
                            CatalogFilters::default(),
                            sort,
                            Seek::Forward(after.clone()),
                            size,
                        )
                        .await;
                        assert!(page.len() <= size as usize);
                        let short = page.len() < size as usize;
                        after = page.last().cloned();
                        walked.extend(page);
                        if short {
                            break;
                        }
                    }
                    assert_eq!(walked, whole, "{sort:?} in pages of {size}");
                }
            }
        }

        #[tokio::test]
        async fn paging_backward_from_the_end_mirrors_paging_forward() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let whole = everything(&fixture, CatalogFilters::default(), sort).await;
                for size in 1..=3_u32 {
                    let mut walked: Vec<CatalogPosition> = Vec::new();
                    let mut before = None;
                    loop {
                        let page = browse(
                            &fixture,
                            CatalogFilters::default(),
                            sort,
                            Seek::Backward(before.clone()),
                            size,
                        )
                        .await;
                        assert!(page.len() <= size as usize);
                        let short = page.len() < size as usize;
                        before = page.first().cloned();
                        let mut joined = page;
                        joined.extend(walked);
                        walked = joined;
                        if short {
                            break;
                        }
                    }
                    assert_eq!(walked, whole, "{sort:?} in pages of {size}, backwards");
                }
            }
        }

        /// A position is a value, not an offset: a page from a title that has
        /// since left the listing starts where that title was.
        #[tokio::test]
        async fn a_page_from_a_title_that_left_the_listing_continues_where_it_was() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let whole = everything(&fixture, CatalogFilters::default(), by_title()).await;
            let gone = whole[2].clone();
            assert_eq!(names(std::slice::from_ref(&gone), &made), ["charlie"]);
            // charlie is a movie: its only file is behind its only entry.
            let entries = fixture.movies().find_entries_by_movie_id(gone.id).await.unwrap();
            let files = fixture.files().find_by_movie_entry_id(entries[0].id).await.unwrap();
            fixture
                .files()
                .mark_missing(vec![files[0].id], ::chrono::Utc::now())
                .await
                .unwrap();

            let after = browse(
                &fixture,
                CatalogFilters::default(),
                by_title(),
                Seek::Forward(Some(gone.clone())),
                10,
            )
            .await;
            assert_eq!(names(&after, &made), ["delta", "Echo", "Foxtrot", "golf"]);
            let before = browse(
                &fixture,
                CatalogFilters::default(),
                by_title(),
                Seek::Backward(Some(gone)),
                10,
            )
            .await;
            assert_eq!(names(&before, &made), ["alpha", "Bravo"]);
        }

        #[tokio::test]
        async fn a_backward_page_is_the_last_rows_before_the_position_in_display_order() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let sort = sorted(CatalogSortField::Title, SortDirection::Desc);

            let last_two =
                browse(&fixture, CatalogFilters::default(), sort, Seek::Backward(None), 2).await;
            assert_eq!(names(&last_two, &made), ["Bravo", "alpha"]);

            let whole = everything(&fixture, CatalogFilters::default(), sort).await;
            let before_echo = browse(
                &fixture,
                CatalogFilters::default(),
                sort,
                Seek::Backward(Some(whole[2].clone())),
                1,
            )
            .await;
            assert_eq!(names(&before_echo, &made), ["Foxtrot"]);
        }

        #[tokio::test]
        async fn every_filter_narrows_both_kinds() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let id_of = |name: &str| made.iter().find(|(_, n)| *n == name).unwrap().0;
            let genres = fixture.genres();
            genres
                .set_movie_genres(
                    id_of("alpha"),
                    &["Science Fiction".to_string(), "Drama".to_string()],
                )
                .await
                .unwrap();
            genres
                .set_show_genres(id_of("delta"), &["Science Fiction".to_string()])
                .await
                .unwrap();
            genres
                .set_movie_genres(id_of("charlie"), &["Comedy".to_string()])
                .await
                .unwrap();
            let listed = |filters| listed_names(&fixture, &made, filters);

            assert_eq!(
                listed(CatalogFilters { kind: Some(TitleKind::Show), ..Default::default() }).await,
                ["delta", "Foxtrot", "golf"]
            );
            assert_eq!(
                listed(CatalogFilters { kind: Some(TitleKind::Movie), ..Default::default() }).await,
                ["alpha", "Bravo", "charlie", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { query: Some("ALPH".to_string()), ..Default::default() })
                    .await,
                ["alpha"],
                "the title search ignores case"
            );
            assert_eq!(
                listed(CatalogFilters {
                    genre_slug: Some("science-fiction".to_string()),
                    ..Default::default()
                })
                .await,
                ["alpha", "delta"]
            );
            assert_eq!(
                listed(CatalogFilters { genre_slug: Some("drama".to_string()), ..Default::default() })
                    .await,
                ["alpha"]
            );
            assert_eq!(
                listed(CatalogFilters {
                    genre_slug: Some("science-fiction".to_string()),
                    kind: Some(TitleKind::Movie),
                    ..Default::default()
                })
                .await,
                ["alpha"]
            );
            assert_eq!(
                listed(CatalogFilters { year: Some(2001), ..Default::default() }).await,
                ["alpha", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { year_from: Some(2000), ..Default::default() }).await,
                ["alpha", "delta", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { year_to: Some(2000), ..Default::default() }).await,
                ["charlie", "golf"]
            );
            assert_eq!(
                listed(CatalogFilters {
                    year_from: Some(2000),
                    year_to: Some(2005),
                    ..Default::default()
                })
                .await,
                ["alpha", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { min_rating: Some(70), ..Default::default() }).await,
                ["alpha", "delta"],
                "a show's rating counts as a movie's does"
            );
            assert_eq!(
                listed(CatalogFilters { min_rating: Some(60), ..Default::default() }).await,
                ["alpha", "delta", "Echo", "golf"],
                "a rating exactly at the minimum is kept; an unrated title counts as 0"
            );
        }

        /// Only titles with a present file are browsable or searchable
        /// (issues #179, #183): a title that never had a file, or whose files
        /// are all missing, is on no page and matches no filter -- and comes
        /// back the moment one of its files does.
        #[tokio::test]
        async fn only_titles_with_a_present_file_are_listed() {
            let fixture = $setup().await;
            movie(&fixture, "hotel", None, None, None, false).await;
            show(&fixture, "india", None, None, 1, false).await;
            let kept_movie = movie(&fixture, "juliet", None, None, None, true).await;
            let (kept_show, show_files) = show(&fixture, "kilo", None, None, 2, true).await;
            let movie_file = kept_movie.file.expect("juliet has a file");
            let listed = |filters| {
                let fixture = &fixture;
                async move {
                    everything(fixture, filters, by_title())
                        .await
                        .into_iter()
                        .map(|row| row.id)
                        .collect::<Vec<Uuid>>()
                }
            };

            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_movie.id, kept_show],
                "titles with no file are not listed"
            );
            assert!(
                listed(CatalogFilters { query: Some("hotel".to_string()), ..Default::default() })
                    .await
                    .is_empty(),
                "nor found by searching for them"
            );

            let files = fixture.files();
            files
                .mark_missing(vec![movie_file, show_files[0]], ::chrono::Utc::now())
                .await
                .unwrap();
            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_show],
                "a movie whose only file is missing leaves; a show with another present episode stays"
            );

            files.mark_missing(vec![show_files[1]], ::chrono::Utc::now()).await.unwrap();
            assert!(
                listed(CatalogFilters::default()).await.is_empty(),
                "a show with every episode file missing leaves"
            );
            assert!(
                listed(CatalogFilters { query: Some("juliet".to_string()), ..Default::default() })
                    .await
                    .is_empty(),
                "and is not found by searching for it"
            );
            assert!(
                fixture.movies().find_by_id(kept_movie.id).await.unwrap().is_some(),
                "a hidden title still resolves by id"
            );

            files.restore(movie_file).await.unwrap();
            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_movie.id],
                "listed again when its file returns"
            );
        }

        #[tokio::test]
        async fn a_position_keyed_for_another_sort_is_refused() {
            let fixture = $setup().await;
            let result = fixture
                .repo()
                .browse(&CatalogQuery {
                    filters: CatalogFilters::default(),
                    sort: by_title(),
                    seek: Seek::Forward(Some(CatalogPosition {
                        kind: TitleKind::Movie,
                        id: Uuid::new_v4(),
                        key: SortKey::Year(Some(2001)),
                    })),
                    limit: NonZeroU32::MIN,
                })
                .await;
            assert!(result.is_err());
        }
    };
}

/// Behavioural contract for [`crate::repositories::GenreRepository`]: genres
/// are shared by slug, a title's set is replaced whole, and a page of titles
/// reads its genre names in one sorted list per title.
///
/// `$setup` names an `async fn() -> impl GenreRepositoryFixture`.
#[macro_export]
macro_rules! genre_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::movie::CreateMovie;
        use $crate::models::show::CreateShow;
        use $crate::repositories::contract::fixture::GenreRepositoryFixture;

        async fn new_movie(fixture: &impl GenreRepositoryFixture) -> Uuid {
            fixture
                .movies()
                .find_or_create_by_identity(CreateMovie::new(
                    format!("genre movie {}", Uuid::new_v4()),
                    None,
                    None,
                ))
                .await
                .unwrap()
                .id
        }

        async fn new_show(fixture: &impl GenreRepositoryFixture) -> Uuid {
            fixture
                .shows()
                .find_or_create_by_identity(CreateShow::new(
                    format!("genre show {}", Uuid::new_v4()),
                    None,
                ))
                .await
                .unwrap()
                .id
        }

        fn owned(names: &[&str]) -> Vec<String> {
            names.iter().map(|n| n.to_string()).collect()
        }

        #[tokio::test]
        async fn each_title_reads_its_own_genres_sorted_ignoring_case() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let first = new_movie(&fixture).await;
            let second = new_movie(&fixture).await;
            let bare = new_movie(&fixture).await;
            let series = new_show(&fixture).await;
            repo.set_movie_genres(first, &owned(&["thriller", "Action", "Drama"]))
                .await
                .unwrap();
            repo.set_movie_genres(second, &owned(&["Comedy"]))
                .await
                .unwrap();
            repo.set_show_genres(series, &owned(&["Mystery", "animation"]))
                .await
                .unwrap();

            let movies = repo
                .movie_genre_names(&[first, second, bare])
                .await
                .unwrap();
            assert_eq!(
                movies.get(&first).unwrap(),
                &owned(&["Action", "Drama", "thriller"])
            );
            assert_eq!(movies.get(&second).unwrap(), &owned(&["Comedy"]));
            assert!(
                !movies.contains_key(&bare),
                "a title without genres is absent"
            );

            let shows = repo.show_genre_names(&[series, first]).await.unwrap();
            assert_eq!(
                shows.get(&series).unwrap(),
                &owned(&["animation", "Mystery"])
            );
            assert!(!shows.contains_key(&first), "a movie is not read as a show");

            assert!(repo.movie_genre_names(&[]).await.unwrap().is_empty());
            assert!(repo.show_genre_names(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn setting_genres_replaces_the_set_and_shares_one_genre_per_slug() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let series = new_show(&fixture).await;
            let unique = Uuid::new_v4().simple().to_string();
            let sci_fi = format!("Science Fiction {unique}");

            repo.set_movie_genres(movie, &[sci_fi.clone(), "Drama".to_string()])
                .await
                .unwrap();
            repo.set_movie_genres(movie, std::slice::from_ref(&sci_fi))
                .await
                .unwrap();
            repo.set_show_genres(series, &[format!("science-fiction {unique}")])
                .await
                .unwrap();

            assert_eq!(
                repo.movie_genre_names(&[movie])
                    .await
                    .unwrap()
                    .get(&movie)
                    .unwrap(),
                &vec![sci_fi.clone()],
                "the second set replaced the first"
            );
            assert_eq!(
                repo.show_genre_names(&[series])
                    .await
                    .unwrap()
                    .get(&series)
                    .unwrap(),
                &vec![sci_fi.clone()],
                "a name with the same slug is the genre already stored"
            );
            let slug = $crate::repositories::genre::slugify(&sci_fi);
            assert_eq!(
                repo.find_all()
                    .await
                    .unwrap()
                    .iter()
                    .filter(|g| g.slug == slug)
                    .count(),
                1
            );
        }
    };
}
