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

    use crate::repositories::{PlaybackProgressRepository, ShowRepository};
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

    /// Everything the [`crate::show_repository_contract`] suite needs.
    ///
    /// Narrower than [`PlaybackProgressFixture`]: the suite builds its own
    /// shows, seasons and episodes through the trait under test, so a backing
    /// store has no rows to seed first.
    pub trait ShowFixture: Send + Sync {
        /// The repository under contract, holding no shows the suite did not
        /// create itself.
        fn repo(&self) -> &dyn ShowRepository;
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

/// Behavioural contract for the find-or-create half of
/// [`crate::repositories::ShowRepository`].
///
/// `$setup` names an `async fn() -> impl ShowFixture`.
///
/// The subject is [`ShowRepository::find_or_create_episode`], and the reason
/// this is a *shared* contract rather than a unit test is that the behaviour it
/// pins is a database constraint. `episodes` carries `idx_episodes_unique` over
/// `(season_id, episode_number)`, so an unconditional insert raises a
/// uniqueness violation and aborts the scan (#142). An in-memory double has no
/// such index and cannot fail that way, so a test that ran only against the
/// fake would pass while production stayed broken.
#[macro_export]
macro_rules! show_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use $crate::models::show::{CreateEpisode, CreateShow};
        use $crate::repositories::contract::fixture::ShowFixture as _;

        /// A season to hang episodes off, created through the trait so every
        /// implementation builds it the same way.
        async fn season_of(
            repo: &dyn $crate::repositories::ShowRepository,
            show_title: &str,
        ) -> ::uuid::Uuid {
            let show = repo
                .create(CreateShow {
                    title: show_title.to_string(),
                    year: Some(2026),
                })
                .await
                .expect("create a show");
            repo.find_or_create_season(show.id, 1)
                .await
                .expect("create season 1")
                .id
        }

        fn episode(season_id: ::uuid::Uuid, number: u32, title: &str) -> CreateEpisode {
            CreateEpisode {
                season_id,
                episode_number: number,
                title: title.to_string(),
                runtime: Some(Duration::from_secs(1_200)),
            }
        }

        /// The #142 regression. A second file for one episode must resolve to
        /// the row the first file created, not raise a uniqueness violation.
        #[tokio::test]
        async fn find_or_create_episode_reuses_the_row_for_a_second_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let season = season_of(repo, "Reused Row").await;

            let first = repo
                .find_or_create_episode(episode(season, 3, "Pilot"))
                .await
                .expect("first file creates the episode");
            let second = repo
                .find_or_create_episode(episode(season, 3, "Pilot"))
                .await
                .expect("a second file for the same episode must not fail the scan");

            assert_eq!(first.id, second.id, "both files resolve to one episode");
            assert_eq!(
                repo.find_episodes_by_season_id(season).await.unwrap().len(),
                1,
                "no duplicate episode row was inserted"
            );
        }

        /// `create` carries only what a filename parse produced, while the
        /// stored row may already hold what enrichment wrote. The weaker source
        /// must not overwrite the stronger one.
        #[tokio::test]
        async fn find_or_create_episode_preserves_the_established_title_and_runtime() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let season = season_of(repo, "Preserved Metadata").await;

            repo.find_or_create_episode(episode(season, 1, "The Canonical Title"))
                .await
                .unwrap();

            let second = repo
                .find_or_create_episode(CreateEpisode {
                    season_id: season,
                    episode_number: 1,
                    title: "Show.S01E01.1080p.WEB-DL".to_string(),
                    runtime: Some(Duration::from_secs(9_999)),
                })
                .await
                .unwrap();

            assert_eq!(
                second.title, "The Canonical Title",
                "a second file's parsed title must not overwrite the stored one"
            );
            assert_eq!(
                second.runtime,
                Some(Duration::from_secs(1_200)),
                "nor its runtime"
            );
        }

        /// Find-or-create must key on the episode number, not merely on the
        /// season -- otherwise every episode after the first would collapse
        /// into one row.
        #[tokio::test]
        async fn find_or_create_episode_keeps_distinct_numbers_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let season = season_of(repo, "Distinct Numbers").await;

            let one = repo
                .find_or_create_episode(episode(season, 1, "One"))
                .await
                .unwrap();
            let two = repo
                .find_or_create_episode(episode(season, 2, "Two"))
                .await
                .unwrap();

            assert_ne!(one.id, two.id);
            assert_eq!(
                repo.find_episodes_by_season_id(season).await.unwrap().len(),
                2
            );
        }

        /// ...and on the season, not merely on the number: episode 1 exists in
        /// every season of every show.
        #[tokio::test]
        async fn find_or_create_episode_scopes_the_number_to_its_season() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let first_season = season_of(repo, "Scoped A").await;
            let second_season = season_of(repo, "Scoped B").await;

            let a = repo
                .find_or_create_episode(episode(first_season, 1, "A1"))
                .await
                .unwrap();
            let b = repo
                .find_or_create_episode(episode(second_season, 1, "B1"))
                .await
                .unwrap();

            assert_ne!(
                a.id, b.id,
                "the same episode number in another season is another episode"
            );
            assert_eq!(
                repo.find_episodes_by_season_id(first_season)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }

        /// The season half of the same behaviour, previously unconstrained by
        /// any shared contract.
        #[tokio::test]
        async fn find_or_create_season_reuses_the_row_for_a_second_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let show = repo
                .create(CreateShow {
                    title: "Reused Season".to_string(),
                    year: None,
                })
                .await
                .unwrap();

            let first = repo.find_or_create_season(show.id, 2).await.unwrap();
            let second = repo.find_or_create_season(show.id, 2).await.unwrap();

            assert_eq!(first.id, second.id);
            assert_eq!(
                repo.find_seasons_by_show_id(show.id).await.unwrap().len(),
                1
            );
        }
    };
}
