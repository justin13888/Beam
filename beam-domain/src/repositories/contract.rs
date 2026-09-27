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
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFile, MediaFileContent};
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
                file_in(&fixture, library, MediaFileContent::Episode { episode_id }).await;
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
        use $crate::models::show::{CreateEpisode, CreateShow, ShowSearchQuery};
        use $crate::providers::enrichment::ShowEnrichment;
        use $crate::repositories::ShowRepository;
        use $crate::repositories::contract::fixture::ShowRepositoryFixture;

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
                    content: Some(MediaFileContent::Episode { episode_id }),
                    status: FileStatus::Known,
                })
                .await
                .expect("create an episode file")
        }

        /// Whether a filterless search lists `show_id`.
        async fn listed(repo: &dyn ShowRepository, show_id: Uuid) -> bool {
            repo.search(&ShowSearchQuery::default())
                .await
                .unwrap()
                .iter()
                .any(|s| s.id == show_id)
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
            }
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
            assert_eq!(found.identity_key, show.identity_key);
        }

        #[tokio::test]
        async fn search_lists_only_shows_with_a_present_episode_file() {
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

            assert!(
                !listed(repo, show).await,
                "a show with no file is not listed"
            );

            let file = episode_file(&fixture, ep.id).await;
            assert!(listed(repo, show).await, "a present file makes it listed");

            fixture
                .files()
                .mark_missing(vec![file.id], ::chrono::Utc::now())
                .await
                .unwrap();
            assert!(
                !listed(repo, show).await,
                "hidden once its only file is missing"
            );
            assert!(
                repo.find_by_id(show).await.unwrap().is_some(),
                "a read by id still resolves the hidden show"
            );

            fixture.files().restore(file.id).await.unwrap();
            assert!(
                listed(repo, show).await,
                "listed again when the file returns"
            );
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
                repo.assign_identity_key(legacy, &parsed.identity_key)
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
                    .assign_identity_key(legacy, "another key|")
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
                    .assign_identity_key(legacy, keyed.identity_key.as_deref().unwrap())
                    .await
                    .unwrap(),
                "a key another show holds is refused"
            );
            assert_eq!(
                repo.find_by_id(legacy).await.unwrap().unwrap().identity_key,
                None
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
        use $crate::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieSearchQuery};
        use $crate::providers::enrichment::MovieEnrichment;
        use $crate::repositories::MovieRepository;
        use $crate::repositories::contract::fixture::MovieRepositoryFixture;

        /// A movie parsed as a title of its own -- a fresh UUID keeps tests
        /// apart -- released in `year`.
        fn parsed(name: &str, year: Option<u32>) -> CreateMovie {
            CreateMovie::new(format!("{name} {}", Uuid::new_v4()), year, None)
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
                .create_entry(CreateMovieEntry {
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
                    })
                    .await
                    .expect("create a movie file"),
            )
        }

        async fn listed(repo: &dyn MovieRepository, movie_id: Uuid) -> bool {
            repo.search(&MovieSearchQuery::default())
                .await
                .unwrap()
                .iter()
                .any(|m| m.id == movie_id)
        }

        fn after_everything() -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::Utc::now() + ::chrono::Duration::minutes(1)
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
        async fn search_lists_only_movies_with_a_present_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let fileless = repo
                .find_or_create_by_identity(parsed("Fileless", None))
                .await
                .unwrap();
            entry_for(&fixture, &fileless, false).await;
            let movie = repo
                .find_or_create_by_identity(parsed("Present", None))
                .await
                .unwrap();
            let file = entry_for(&fixture, &movie, true).await.unwrap();

            assert!(
                !listed(repo, fileless.id).await,
                "a movie with no file is not listed"
            );
            assert!(listed(repo, movie.id).await);

            fixture
                .files()
                .mark_missing(vec![file.id], ::chrono::Utc::now())
                .await
                .unwrap();
            assert!(
                !listed(repo, movie.id).await,
                "hidden once its only file is missing"
            );
            assert!(
                repo.find_by_id(movie.id).await.unwrap().is_some(),
                "a read by id still resolves the hidden movie"
            );

            fixture.files().restore(file.id).await.unwrap();
            assert!(
                listed(repo, movie.id).await,
                "listed again when the file returns"
            );
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
                repo.assign_identity_key(legacy, &create.identity_key)
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
                    .assign_identity_key(legacy, "another key|")
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
                    .assign_identity_key(legacy, &create.identity_key)
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
                    .assign_identity_key(Uuid::new_v4(), "nobody|")
                    .await
                    .unwrap(),
                "an unknown movie is not keyed"
            );
        }
    };
}
