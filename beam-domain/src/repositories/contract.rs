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
    };
}
