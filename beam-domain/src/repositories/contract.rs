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

    use crate::models::watch_state::WatchTarget;
    use crate::repositories::WatchStateRepository;
    use crate::services::TestClock;

    /// Everything the [`crate::watch_state_repository_contract`] suite needs
    /// from a backing store.
    ///
    /// Identifiers are allocated by the fixture rather than invented by the
    /// contract because a real Postgres enforces the user, movie, episode,
    /// show and file foreign keys: the in-memory fixture can hand back a bare
    /// [`Uuid::new_v4`], while the Postgres fixture must insert the referenced
    /// rows first. The contract itself stays identical across both.
    #[async_trait::async_trait]
    pub trait WatchStateFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn WatchStateRepository;

        /// The clock the repository stamps `last_played_at` from.
        fn clock(&self) -> &TestClock;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;

        /// A movie, as the target a row of it is keyed by.
        async fn new_movie(&self) -> WatchTarget;

        /// A show with no episodes yet.
        async fn new_show(&self) -> Uuid;

        /// A further episode of `show_id`.
        async fn new_episode(&self, show_id: Uuid) -> WatchTarget;

        /// A media file a report can name.
        async fn new_file(&self) -> Uuid;
    }

    /// Everything the [`crate::file_repository_contract`] suite needs from a
    /// backing store.
    ///
    /// The parents a file row hangs off are allocated by the fixture for the
    /// same reason as in [`WatchStateFixture`]: Postgres enforces the
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

    /// Everything the [`crate::applied_nfo_repository_contract`] suite needs
    /// from a backing store: the libraries a record hangs off, which Postgres
    /// holds to a foreign key.
    #[async_trait::async_trait]
    pub trait AppliedNfoFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::AppliedNfoRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::sidecar_subtitle_repository_contract`] suite
    /// needs from a backing store: the libraries and video files a subtitle
    /// row hangs off, which Postgres holds to foreign keys.
    #[async_trait::async_trait]
    pub trait SidecarSubtitleFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::SidecarSubtitleRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A video file in `library_id`.
        async fn new_video_file(&self, library_id: Uuid) -> Uuid;
    }

    /// Everything the [`crate::enrichment_state_repository_contract`] suite
    /// needs: the repository under contract and the title repositories over
    /// the same store, since a real Postgres holds a row to its title by a
    /// foreign key. Listing, counting and refreshing everything are global,
    /// so a Postgres fixture must give each test a store of its own.
    #[async_trait::async_trait]
    pub trait EnrichmentStateFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::EnrichmentStateRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }
}

/// Behavioural contract for [`crate::repositories::WatchStateRepository`].
///
/// `$setup` names an `async fn() -> impl WatchStateFixture`.
#[macro_export]
macro_rules! watch_state_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::models::watch_state::{
            ContinueCandidate, HistoryPosition, RecordProgress, TitleRef, WatchState, WatchTarget,
        };
        use $crate::repositories::contract::fixture::WatchStateFixture as _;

        /// The titles of `candidates`, in order.
        fn titles(candidates: Vec<ContinueCandidate>) -> Vec<TitleRef> {
            candidates.into_iter().map(|c| c.title).collect()
        }

        /// One report into a 100-second title, so the 95% threshold sits at
        /// 95.0.
        fn report(
            user_id: Uuid,
            target: WatchTarget,
            file_id: Uuid,
            position_secs: f64,
        ) -> RecordProgress {
            RecordProgress {
                user_id,
                target,
                file_id,
                position_secs,
                duration_secs: Some(100.0),
            }
        }

        fn key(target: WatchTarget) -> Uuid {
            match target {
                WatchTarget::Movie { movie_id } => movie_id,
                WatchTarget::Episode { episode_id, .. } => episode_id,
            }
        }

        fn state(row: &WatchState) -> (f64, bool, u32) {
            (row.position_secs, row.completed, row.play_count)
        }

        #[tokio::test]
        async fn two_sources_of_one_title_share_one_row_that_follows_the_last_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let (hd, uhd) = (fixture.new_file().await, fixture.new_file().await);

            let first = repo
                .record_progress(report(user, movie, hd, 40.0))
                .await
                .unwrap();
            let read = repo
                .find(user, movie)
                .await
                .unwrap()
                .expect("the row just written");
            assert_eq!(read.position_secs, 40.0, "the other source resumes here");
            let second = repo
                .record_progress(report(user, movie, uhd, 50.0))
                .await
                .unwrap();

            assert_eq!(first.id, second.id, "one row per title, not per file");
            assert_eq!(second.last_file_id, Some(uhd));
            assert_eq!(second.target, movie);
            assert_eq!(repo.count_by_user(user).await.unwrap(), 1);
        }

        #[tokio::test]
        async fn reaching_the_end_marks_played_at_the_start_and_counts_one_play() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let file = fixture.new_file().await;

            let below = repo
                .record_progress(report(user, movie, file, 94.9))
                .await
                .unwrap();
            assert_eq!(state(&below), (94.9, false, 0), "94.9% is short of the end");
            let at = repo
                .record_progress(report(user, movie, file, 95.0))
                .await
                .unwrap();
            assert_eq!(
                state(&at),
                (0.0, true, 1),
                "the threshold itself is the end"
            );
            let again = repo
                .record_progress(report(user, movie, file, 97.0))
                .await
                .unwrap();
            assert_eq!(
                state(&again),
                (0.0, true, 1),
                "a second report past the end is not a second play"
            );
        }

        #[tokio::test]
        async fn a_rewind_after_the_end_is_a_rewatch_never_an_unplay() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let file = fixture.new_file().await;

            repo.record_progress(report(user, movie, file, 99.0))
                .await
                .unwrap();
            let rewound = repo
                .record_progress(report(user, movie, file, 5.0))
                .await
                .unwrap();
            assert_eq!(state(&rewound), (5.0, true, 1), "played stays played");
            let rewatched = repo
                .record_progress(report(user, movie, file, 96.0))
                .await
                .unwrap();
            assert_eq!(
                state(&rewatched),
                (0.0, true, 2),
                "a rewatch to the end counts"
            );
        }

        #[tokio::test]
        async fn a_report_with_no_duration_keeps_the_last_known_one() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let file = fixture.new_file().await;

            repo.record_progress(report(user, movie, file, 10.0))
                .await
                .unwrap();
            let unknown = repo
                .record_progress(RecordProgress {
                    duration_secs: None,
                    ..report(user, movie, file, 1_000.0)
                })
                .await
                .unwrap();
            assert_eq!(unknown.duration_secs, Some(100.0));
            assert_eq!(state(&unknown), (1_000.0, false, 0), "no duration, no end");
        }

        #[tokio::test]
        async fn rows_are_kept_per_user() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (alice, bob) = (fixture.new_user().await, fixture.new_user().await);
            let show = fixture.new_show().await;
            let episode = fixture.new_episode(show).await;
            let file = fixture.new_file().await;

            repo.record_progress(report(alice, episode, file, 10.0))
                .await
                .unwrap();
            repo.record_progress(report(bob, episode, file, 30.0))
                .await
                .unwrap();

            let position = |row: Option<WatchState>| row.expect("a row").position_secs;
            assert_eq!(position(repo.find(alice, episode).await.unwrap()), 10.0);
            assert_eq!(position(repo.find(bob, episode).await.unwrap()), 30.0);
            assert_eq!(repo.find_for_show(alice, show).await.unwrap().len(), 1);
            assert_eq!(repo.count_by_user(alice).await.unwrap(), 1);
        }

        #[tokio::test]
        async fn every_write_is_stamped_by_the_injected_clock() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let file = fixture.new_file().await;

            let first = repo
                .record_progress(report(user, movie, file, 10.0))
                .await
                .unwrap();
            fixture.clock().advance(Duration::from_secs(3600));
            let second = repo
                .record_progress(report(user, movie, file, 20.0))
                .await
                .unwrap();
            assert_eq!(
                (second.last_played_at - first.last_played_at).num_seconds(),
                3600
            );
        }

        #[tokio::test]
        async fn marking_played_is_idempotent_and_stamps_every_target_alike() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let show = fixture.new_show().await;
            let started = fixture.new_episode(show).await;
            let fresh = fixture.new_episode(show).await;
            let file = fixture.new_file().await;
            repo.record_progress(report(user, started, file, 30.0))
                .await
                .unwrap();

            clock.advance(Duration::from_secs(60));
            repo.mark_played(user, &[started, fresh]).await.unwrap();
            let rows = repo.find_for_show(user, show).await.unwrap();
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert_eq!(state(row), (0.0, true, 1), "{:?}", row.target);
            }
            assert_eq!(rows[0].last_played_at, rows[1].last_played_at);
            let marked_at = rows[0].last_played_at;

            clock.advance(Duration::from_secs(60));
            repo.mark_played(user, &[started, fresh]).await.unwrap();
            for row in repo.find_for_show(user, show).await.unwrap() {
                assert_eq!(state(&row), (0.0, true, 1), "marking again changes nothing");
                assert_eq!(row.last_played_at, marked_at, "not even when it was played");
            }
            assert_eq!(
                repo.find(user, started)
                    .await
                    .unwrap()
                    .expect("a row")
                    .last_file_id,
                Some(file),
                "a mark keeps the file the viewer last played"
            );
        }

        #[tokio::test]
        async fn marking_nothing_writes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            repo.mark_played(user, &[]).await.unwrap();
            repo.mark_unplayed(user, &[]).await.unwrap();
            assert_eq!(repo.count_by_user(user).await.unwrap(), 0);
        }

        #[tokio::test]
        async fn marking_unplayed_forgets_the_title_entirely() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let other = fixture.new_movie().await;
            let file = fixture.new_file().await;
            repo.record_progress(report(user, movie, file, 99.0))
                .await
                .unwrap();
            repo.record_progress(report(user, movie, file, 20.0))
                .await
                .unwrap();
            repo.mark_played(user, &[other]).await.unwrap();

            repo.mark_unplayed(user, &[movie]).await.unwrap();

            assert!(repo.find(user, movie).await.unwrap().is_none());
            assert!(
                repo.find(user, other).await.unwrap().is_some(),
                "only the named title"
            );
        }

        #[tokio::test]
        async fn clearing_progress_forgets_an_unplayed_title_and_rewinds_a_played_one() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let started = fixture.new_movie().await;
            let rewatching = fixture.new_movie().await;
            let file = fixture.new_file().await;
            repo.record_progress(report(user, started, file, 40.0))
                .await
                .unwrap();
            repo.record_progress(report(user, rewatching, file, 99.0))
                .await
                .unwrap();
            repo.record_progress(report(user, rewatching, file, 40.0))
                .await
                .unwrap();

            repo.clear_progress(user, started).await.unwrap();
            repo.clear_progress(user, rewatching).await.unwrap();

            assert!(repo.find(user, started).await.unwrap().is_none());
            let kept = repo
                .find(user, rewatching)
                .await
                .unwrap()
                .expect("played is kept");
            assert_eq!(state(&kept), (0.0, true, 1));
        }

        /// Two titles the indexer merges into one keep their viewers' state
        /// on the one kept: a lone row moves, and two rows of one viewer fold
        /// into one, the newer's place with the plays of both.
        #[tokio::test]
        async fn carrying_moves_a_retired_titles_rows_onto_the_kept_one() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let (alone, older_kept, newer_gone) = (
                fixture.new_user().await,
                fixture.new_user().await,
                fixture.new_user().await,
            );
            let (gone, kept) = (fixture.new_movie().await, fixture.new_movie().await);
            let (old_file, new_file) = (fixture.new_file().await, fixture.new_file().await);

            repo.record_progress(report(alone, gone, old_file, 40.0))
                .await
                .unwrap();
            repo.record_progress(report(older_kept, gone, old_file, 99.0))
                .await
                .unwrap();
            repo.record_progress(report(newer_gone, kept, old_file, 20.0))
                .await
                .unwrap();
            repo.dismiss(newer_gone, TitleRef::Movie(key(kept)))
                .await
                .unwrap();
            clock.advance(Duration::from_secs(60));
            repo.record_progress(report(older_kept, kept, new_file, 30.0))
                .await
                .unwrap();
            repo.record_progress(report(newer_gone, gone, new_file, 50.0))
                .await
                .unwrap();
            let later = repo.find(newer_gone, gone).await.unwrap().expect("a row");

            repo.carry(gone, kept).await.unwrap();

            for user in [alone, older_kept, newer_gone] {
                assert!(repo.find(user, gone).await.unwrap().is_none());
            }
            let moved = repo.find(alone, kept).await.unwrap().expect("moved");
            assert_eq!(state(&moved), (40.0, false, 0));
            assert_eq!(moved.last_file_id, Some(old_file));

            let folded = repo.find(older_kept, kept).await.unwrap().expect("kept");
            assert_eq!(
                state(&folded),
                (30.0, true, 1),
                "the newer place, played as the retired row was"
            );
            assert_eq!(folded.last_file_id, Some(new_file));
            assert_eq!(repo.count_by_user(older_kept).await.unwrap(), 1);

            let taken = repo.find(newer_gone, kept).await.unwrap().expect("kept");
            assert_eq!(state(&taken), (50.0, false, 0), "the retired row was newer");
            assert_eq!(taken.last_file_id, Some(new_file));
            assert_eq!(taken.last_played_at, later.last_played_at);
            assert!(
                taken.dismissed_at.is_some(),
                "the kept row's dismissal stays"
            );
            assert_eq!(
                titles(
                    repo.find_continue_candidates(newer_gone, None, 10)
                        .await
                        .unwrap()
                ),
                vec![TitleRef::Movie(key(kept))],
                "played since the dismissal, it is back"
            );

            let show = fixture.new_show().await;
            let other = fixture.new_show().await;
            let (from, to) = (
                fixture.new_episode(show).await,
                fixture.new_episode(other).await,
            );
            repo.record_progress(report(alone, from, old_file, 10.0))
                .await
                .unwrap();
            repo.carry(from, to).await.unwrap();
            let rows = repo.find_for_show(alone, other).await.unwrap();
            assert_eq!(rows.len(), 1, "the episode's row joins the kept show");
            assert_eq!(rows[0].target, to);
            assert!(repo.find_for_show(alone, show).await.unwrap().is_empty());

            assert!(repo.carry(kept, to).await.is_err(), "a movie is no episode");
            repo.carry(kept, kept).await.unwrap();
            assert_eq!(
                state(&repo.find(alone, kept).await.unwrap().expect("unchanged")),
                (40.0, false, 0)
            );
        }

        #[tokio::test]
        async fn candidates_are_one_per_title_newest_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let show = fixture.new_show().await;
            let (e1, e2) = (
                fixture.new_episode(show).await,
                fixture.new_episode(show).await,
            );
            let file = fixture.new_file().await;

            repo.record_progress(report(user, e1, file, 10.0))
                .await
                .unwrap();
            clock.advance(Duration::from_secs(60));
            repo.record_progress(report(user, movie, file, 10.0))
                .await
                .unwrap();
            clock.advance(Duration::from_secs(60));
            repo.record_progress(report(user, e2, file, 10.0))
                .await
                .unwrap();

            let all = repo.find_continue_candidates(user, None, 10).await.unwrap();
            assert_eq!(
                titles(all.clone()),
                vec![TitleRef::Show(show), TitleRef::Movie(key(movie))],
                "the show's newest episode places the show once"
            );
            let newest = repo.find(user, e2).await.unwrap().expect("a row");
            assert_eq!(
                all[0].last_played_at, newest.last_played_at,
                "a show is as recent as its newest episode"
            );
            assert_eq!(
                repo.find_continue_candidates(user, None, 1).await.unwrap(),
                all[..1].to_vec(),
                "the limit cuts the same order"
            );
            assert_eq!(
                repo.find_continue_candidates(user, Some(all[0]), 10)
                    .await
                    .unwrap(),
                all[1..].to_vec(),
                "a page resumes after the candidate it names"
            );
            assert!(
                repo.find_continue_candidates(user, Some(all[1]), 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        /// A bulk mark stamps every title it touches with one instant, so
        /// paging must break the tie by the title's id: read one at a time,
        /// the pages are the whole list once each, in the same order.
        #[tokio::test]
        async fn candidates_played_at_one_instant_page_by_title_id_without_repeats() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let file = fixture.new_file().await;
            let mut episodes = Vec::new();
            for _ in 0..5 {
                let show = fixture.new_show().await;
                episodes.push(fixture.new_episode(show).await);
            }
            let movie = fixture.new_movie().await;
            repo.record_progress(report(user, movie, file, 10.0))
                .await
                .unwrap();
            clock.advance(Duration::from_secs(60));
            repo.mark_played(user, &episodes).await.unwrap();

            let all = repo.find_continue_candidates(user, None, 10).await.unwrap();
            assert_eq!(all.len(), 6);
            let tied: Vec<Uuid> = all[..5].iter().map(|c| c.title.id()).collect();
            let mut descending = tied.clone();
            descending.sort_by(|a, b| b.cmp(a));
            assert_eq!(tied, descending, "a tie falls to the larger id first");
            assert_eq!(all[5].title, TitleRef::Movie(key(movie)));

            let mut paged = Vec::new();
            let mut after = None;
            loop {
                let page = repo.find_continue_candidates(user, after, 1).await.unwrap();
                let Some(last) = page.last().copied() else {
                    break;
                };
                paged.extend(page);
                after = Some(last);
            }
            assert_eq!(paged, all);
        }

        #[tokio::test]
        async fn rows_of_named_shows_are_read_together() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (user, other) = (fixture.new_user().await, fixture.new_user().await);
            let file = fixture.new_file().await;
            let (a, b, c) = (
                fixture.new_show().await,
                fixture.new_show().await,
                fixture.new_show().await,
            );
            let (a1, a2, b1, c1) = (
                fixture.new_episode(a).await,
                fixture.new_episode(a).await,
                fixture.new_episode(b).await,
                fixture.new_episode(c).await,
            );
            let movie = fixture.new_movie().await;
            for target in [a1, a2, b1, c1, movie] {
                repo.record_progress(report(user, target, file, 10.0))
                    .await
                    .unwrap();
            }
            repo.record_progress(report(other, a1, file, 10.0))
                .await
                .unwrap();

            let mut read: Vec<Uuid> = repo
                .find_for_shows(user, &[a, b])
                .await
                .unwrap()
                .into_iter()
                .inspect(|row| assert_eq!(row.user_id, user))
                .map(|row| key(row.target))
                .collect();
            read.sort();
            let mut expected = vec![key(a1), key(a2), key(b1)];
            expected.sort();
            assert_eq!(read, expected, "every episode of the named shows, no other");
            assert!(repo.find_for_shows(user, &[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn a_finished_movie_is_no_candidate_but_a_finished_episode_leads_on() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = fixture.new_movie().await;
            let show = fixture.new_show().await;
            let episode = fixture.new_episode(show).await;
            let file = fixture.new_file().await;
            repo.record_progress(report(user, movie, file, 99.0))
                .await
                .unwrap();
            repo.record_progress(report(user, episode, file, 99.0))
                .await
                .unwrap();

            assert_eq!(
                titles(repo.find_continue_candidates(user, None, 10).await.unwrap()),
                vec![TitleRef::Show(show)]
            );
        }

        #[tokio::test]
        async fn a_dismissed_title_returns_only_when_played_again() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let show = fixture.new_show().await;
            let (e1, e2) = (
                fixture.new_episode(show).await,
                fixture.new_episode(show).await,
            );
            let movie = fixture.new_movie().await;
            let file = fixture.new_file().await;
            repo.record_progress(report(user, e1, file, 10.0))
                .await
                .unwrap();
            repo.record_progress(report(user, movie, file, 10.0))
                .await
                .unwrap();

            repo.dismiss(user, TitleRef::Show(show)).await.unwrap();
            assert_eq!(
                titles(repo.find_continue_candidates(user, None, 10).await.unwrap()),
                vec![TitleRef::Movie(key(movie))]
            );
            assert_eq!(
                repo.find(user, e1)
                    .await
                    .unwrap()
                    .expect("a row")
                    .position_secs,
                10.0,
                "dismissing hides the title; it keeps the resume point"
            );

            clock.advance(Duration::from_secs(60));
            repo.record_progress(report(user, e2, file, 10.0))
                .await
                .unwrap();
            assert_eq!(
                titles(repo.find_continue_candidates(user, None, 10).await.unwrap()),
                vec![TitleRef::Show(show), TitleRef::Movie(key(movie))],
                "any episode played after the dismissal brings the show back"
            );
        }

        #[tokio::test]
        async fn history_pages_newest_first_after_a_position() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let clock = fixture.clock();
            let user = fixture.new_user().await;
            let other = fixture.new_user().await;
            let file = fixture.new_file().await;
            let mut movies = Vec::new();
            for _ in 0..4 {
                let movie = fixture.new_movie().await;
                repo.record_progress(report(user, movie, file, 99.0))
                    .await
                    .unwrap();
                clock.advance(Duration::from_secs(60));
                movies.push(movie);
            }
            let theirs = fixture.new_movie().await;
            repo.record_progress(report(other, theirs, file, 10.0))
                .await
                .unwrap();

            let targets = |rows: &[WatchState]| rows.iter().map(|r| r.target).collect::<Vec<_>>();
            let first = repo.find_history_page(user, None, 2).await.unwrap();
            assert_eq!(targets(&first), vec![movies[3], movies[2]]);
            let after = first.last().map(HistoryPosition::from);
            let second = repo.find_history_page(user, after, 2).await.unwrap();
            assert_eq!(targets(&second), vec![movies[1], movies[0]]);
            let after = second.last().map(HistoryPosition::from);
            assert!(
                repo.find_history_page(user, after, 2)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(repo.count_by_user(user).await.unwrap(), 4);
        }

        #[tokio::test]
        async fn history_rows_played_at_one_instant_page_without_a_gap_or_a_repeat() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let show = fixture.new_show().await;
            let mut episodes = Vec::new();
            for _ in 0..5 {
                episodes.push(fixture.new_episode(show).await);
            }
            repo.mark_played(user, &episodes).await.unwrap();

            let mut seen = Vec::new();
            let mut after = None;
            loop {
                let page = repo.find_history_page(user, after, 2).await.unwrap();
                let Some(last) = page.last() else { break };
                after = Some(HistoryPosition::from(last));
                seen.extend(page.iter().map(|r| r.target));
            }
            seen.sort_by_key(|t| key(*t));
            let mut expected = episodes.clone();
            expected.sort_by_key(|t| key(*t));
            assert_eq!(seen, expected);
        }

        #[tokio::test]
        async fn find_for_movies_reads_only_the_movies_asked() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let (asked, unasked) = (fixture.new_movie().await, fixture.new_movie().await);
            repo.mark_played(user, &[asked, unasked]).await.unwrap();

            let rows = repo.find_for_movies(user, &[key(asked)]).await.unwrap();
            assert_eq!(
                rows.iter().map(|r| r.target).collect::<Vec<_>>(),
                vec![asked]
            );
            assert!(repo.find_for_movies(user, &[]).await.unwrap().is_empty());
        }

        /// A file's last play is the latest row whose last report named it,
        /// across users; a file never named is absent.
        #[tokio::test]
        async fn last_played_at_is_the_latest_row_naming_each_file() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (alice, bob) = (fixture.new_user().await, fixture.new_user().await);
            let movie = fixture.new_movie().await;
            let (watched, unplayed) = (fixture.new_file().await, fixture.new_file().await);

            let first = repo
                .record_progress(report(alice, movie, watched, 10.0))
                .await
                .unwrap();
            fixture.clock().advance(Duration::from_secs(60));
            let latest = repo
                .record_progress(report(bob, movie, watched, 20.0))
                .await
                .unwrap();

            let last = repo.last_played_at(vec![watched, unplayed]).await.unwrap();
            assert!(latest.last_played_at > first.last_played_at);
            assert_eq!(last.get(&watched), Some(&latest.last_played_at));
            assert_eq!(last.len(), 1, "{last:?}");
            assert!(repo.last_played_at(Vec::new()).await.unwrap().is_empty());
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
        use $crate::utils::classification::ContainerTags;

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
                    identity: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: Some(some_tags()),
                })
                .await
                .expect("create a file")
        }

        /// Tags a probe could have read; some left out, as most files do.
        fn some_tags() -> ContainerTags {
            ContainerTags {
                show: Some("The Office".to_string()),
                season: Some(2),
                ..ContainerTags::default()
            }
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

        /// The files beneath a directory are those whose path starts with it a
        /// whole component at a time, at any depth -- the directory name is a
        /// literal, never a pattern -- and present, in that library.
        #[tokio::test]
        async fn the_files_under_a_directory_are_the_present_ones_beneath_it() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let root = PathBuf::from(format!("/videos/{library}"));
            let mut by_name = ::std::collections::HashMap::new();
            for (name, in_library) in [
                ("Show/a.mkv", library),
                ("Show/Season 1/b.mkv", library),
                ("Show/Season 1/Extras/c.mkv", library),
                ("Show 2/d.mkv", library),
                ("Showtime/e.mkv", library),
                ("Sho_/f.mkv", library),
                ("Sh%/g.mkv", library),
                ("Show/gone.mkv", library),
                ("Show/elsewhere.mkv", other_library),
            ] {
                let movie_entry_id = fixture.new_movie_entry(in_library).await;
                let unique = Uuid::new_v4();
                let file = fixture
                    .repo()
                    .create(CreateMediaFile {
                        library_id: in_library,
                        path: root.join(name),
                        hash: (unique.as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: None,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::Movie { movie_entry_id }),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                by_name.insert(name, file.id);
            }
            fixture
                .repo()
                .mark_missing(vec![by_name["Show/gone.mkv"]], at(0))
                .await
                .unwrap();
            let under = |dir: &'static str| {
                let dir = root.join(dir);
                let repo = fixture.repo();
                async move { ids(&repo.find_all_under(library, &dir).await.unwrap()) }
            };
            let named = |names: &[&str]| sorted(names.iter().map(|n| by_name[n]).collect());

            assert_eq!(
                under("Show").await,
                named(&[
                    "Show/a.mkv",
                    "Show/Season 1/b.mkv",
                    "Show/Season 1/Extras/c.mkv"
                ]),
                "every present file beneath, at any depth; not `Show 2` or `Showtime`"
            );
            assert_eq!(
                under("Show/Season 1").await,
                named(&["Show/Season 1/b.mkv", "Show/Season 1/Extras/c.mkv"])
            );
            assert_eq!(under("Sho_").await, named(&["Sho_/f.mkv"]), "`_` is literal");
            assert_eq!(under("Sh%").await, named(&["Sh%/g.mkv"]), "`%` is literal");
            assert_eq!(
                under("Show/a.mkv").await,
                Vec::<Uuid>::new(),
                "a file is not beneath itself"
            );
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
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
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

        /// An mtime is kept in whole microseconds, as `files.mtime`'s
        /// `TIMESTAMPTZ` is, and reads back as [`mtime_as_stored`] says --
        /// the precision the indexer brings a file's mtime to before
        /// comparing it with its row (issue #229). Whichever way it is
        /// written: created, updated, or relinked.
        #[tokio::test]
        async fn an_mtime_reads_back_in_whole_microseconds_as_mtime_as_stored_says() {
            use $crate::models::file::mtime_as_stored;
            let instant = |secs: i64, nanos: u32| {
                DateTime::from_timestamp(secs, nanos).expect("valid instant")
            };
            // (written, read back): what ext4 or btrfs reports, and what a
            // row holds for it.
            let cases = [
                // After 2000-01-01, the sub-microsecond part is dropped.
                (instant(1_790_000_341, 802_029_432), instant(1_790_000_341, 802_029_000)),
                (instant(1_790_000_341, 999_999_999), instant(1_790_000_341, 999_999_000)),
                // Before it, the driver truncates toward that epoch: up.
                (instant(946_684_799, 500), instant(946_684_799, 1_000)),
                // Before 1970 as well: the Unix epoch is not the one that counts.
                (instant(-1, 500), instant(-1, 1_000)),
                // Whole microseconds are kept as they are.
                (instant(1_790_000_341, 802_029_000), instant(1_790_000_341, 802_029_000)),
            ];
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;

            for (written, read_back) in cases {
                assert_eq!(mtime_as_stored(written), read_back, "{written}");

                let movie_entry_id = fixture.new_movie_entry(library).await;
                let created = repo
                    .create(CreateMediaFile {
                        library_id: library,
                        path: PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4())),
                        hash: (Uuid::new_v4().as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: Some(written),
                        identity: None,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::Movie { movie_entry_id }),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                let stored = repo.find_by_id(created.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "created at {written}");

                let untouched = movie_file(&fixture, library).await;
                repo.update(UpdateMediaFile {
                    id: untouched.id,
                    hash: None,
                    size_bytes: None,
                    mtime: Some(written),
                    identity: None,
                    probe: ProbeUpdate::Keep,
                    content: None,
                    status: None,
                })
                .await
                .expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "updated to {written}");

                let moving = movie_file(&fixture, library).await;
                let moved_to =
                    PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));
                repo.relink(vec![to(&moving, &moved_to, 1024, Some(written))], Vec::new(), at(0))
                    .await
                    .expect("relink the file");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "relinked at {written}");
            }
        }

        /// A file's identity -- its inode and ctime (issue #228) -- is kept
        /// whichever way it is written: created, updated, or relinked. The
        /// inode keeps all 64 bits, past `i64::MAX` too, and the ctime reads
        /// back as [`FileIdentity::as_stored`] says, as an mtime does. An
        /// update that names none leaves the row's as it was, and a row
        /// written without one reads back without one.
        #[tokio::test]
        async fn an_identity_reads_back_as_stored_however_it_is_written() {
            use $crate::models::file::FileIdentity;
            let instant = |secs: i64, nanos: u32| {
                DateTime::from_timestamp(secs, nanos).expect("valid instant")
            };
            // (written, the ctime it reads back with): nanoseconds as ext4 or
            // btrfs reports them, and whole microseconds as a row keeps them.
            let identities = [
                (
                    FileIdentity { inode: 42, ctime: instant(1_790_000_341, 802_029_432) },
                    instant(1_790_000_341, 802_029_000),
                ),
                (
                    FileIdentity { inode: u64::MAX, ctime: instant(1_790_000_341, 999_999_999) },
                    instant(1_790_000_341, 999_999_000),
                ),
                (FileIdentity { inode: 1 << 63, ctime: at(5) }, at(5)),
            ];
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;

            let without = movie_file(&fixture, library).await;
            let stored = repo.find_by_id(without.id).await.unwrap().expect("stored");
            assert_eq!(stored.identity, None, "created without one");

            for (written, ctime) in identities {
                let read_back = Some(FileIdentity { inode: written.inode, ctime });
                assert_eq!(Some(written.as_stored()), read_back, "{written:?}");

                let movie_entry_id = fixture.new_movie_entry(library).await;
                let created = repo
                    .create(CreateMediaFile {
                        library_id: library,
                        path: PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4())),
                        hash: (Uuid::new_v4().as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: Some(written),
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::Movie { movie_entry_id }),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                let stored = repo.find_by_id(created.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "created with {written:?}");

                let untouched = movie_file(&fixture, library).await;
                let update = |identity: Option<FileIdentity>| UpdateMediaFile {
                    id: untouched.id,
                    hash: None,
                    size_bytes: None,
                    mtime: None,
                    identity,
                    probe: ProbeUpdate::Keep,
                    content: None,
                    status: None,
                };
                repo.update(update(Some(written))).await.expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "updated to {written:?}");
                repo.update(update(None)).await.expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "an update naming none keeps it");

                let moving = movie_file(&fixture, library).await;
                let moved_to =
                    PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));
                let relink = FileRelink {
                    identity: Some(written),
                    ..to(&moving, &moved_to, 1024, None)
                };
                repo.relink(vec![relink], Vec::new(), at(0))
                    .await
                    .expect("relink the file");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "relinked at {written:?}");

                // A relink records what the file was found at, so one found
                // with no identity -- a platform without them -- clears the
                // identity the row had, unlike an update naming none.
                let moved_again =
                    PathBuf::from(format!("/videos/{library}/again/{}.mkv", Uuid::new_v4()));
                repo.relink(vec![to(&stored, &moved_again, 1024, None)], Vec::new(), at(1))
                    .await
                    .expect("relink the file again");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, None, "relinked from {written:?} with none");
            }
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
            assert_eq!(
                stored.container_tags,
                Some(some_tags()),
                "reclassifying reads the tags; it never replaces them"
            );
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
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status,
                    classifier_version: 0,
                    container_tags: None,
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
                        identity: None,
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
                identity: None,
                probe,
                content: None,
                status: None,
            };
            let probe_of = |file: &MediaFile| {
                (
                    file.mime_type.clone(),
                    file.duration,
                    file.container_format.clone(),
                    file.container_tags.clone(),
                )
            };
            let tags = ContainerTags {
                title: Some("The Dundies".to_string()),
                show: Some("The Office".to_string()),
                season: Some(2),
                episode: Some(1),
                year: Some(2005),
            };

            repo.update(update(ProbeUpdate::Set {
                mime_type: "video/mp4".to_string(),
                duration: ::std::time::Duration::from_secs(90),
                container_format: "mp4".to_string(),
                container_tags: tags.clone(),
            }))
            .await
            .expect("set the probe results");
            let expected = (
                Some("video/mp4".to_string()),
                Some(::std::time::Duration::from_secs(90)),
                Some("mp4".to_string()),
                Some(tags),
            );
            let stored = || async {
                repo.find_by_id(file.id)
                    .await
                    .unwrap()
                    .expect("still present")
            };
            assert_eq!(probe_of(&stored().await), expected);

            repo.update(update(ProbeUpdate::Keep))
                .await
                .expect("keep the probe results");
            assert_eq!(probe_of(&stored().await), expected);

            repo.update(update(ProbeUpdate::Clear))
                .await
                .expect("clear the probe results");
            let cleared = stored().await;
            assert_eq!(probe_of(&cleared), (None, None, None, None));
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
            assert_eq!(
                found.container_tags,
                Some(some_tags()),
                "the tags it was created with are stored"
            );
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
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
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
                identity: None,
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
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::Movie { movie_entry_id }),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
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
                    identity: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(MediaFileContent::episode(episode_id)),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
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

        /// Next-up walks the outline (issue #188): every episode of the show
        /// and no other's, in (season, episode) order with the specials
        /// first, each playable exactly when a present file backs it.
        #[tokio::test]
        async fn the_episode_outline_orders_the_show_and_marks_what_can_play() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let show = repo
                .find_or_create_by_identity(new_show("outline"))
                .await
                .unwrap();
            let specials = repo.find_or_create_season(show.id, 0).await.unwrap();
            let two = repo.find_or_create_season(show.id, 2).await.unwrap();
            let one = repo.find_or_create_season(show.id, 1).await.unwrap();
            let s2e1 = repo
                .find_or_create_episode(episode(two.id, 1, "S2E1", 30))
                .await
                .unwrap();
            let s1e2 = repo
                .find_or_create_episode(episode(one.id, 2, "S1E2", 30))
                .await
                .unwrap();
            let s1e1 = repo
                .find_or_create_episode(episode(one.id, 1, "S1E1", 30))
                .await
                .unwrap();
            let s0e1 = repo
                .find_or_create_episode(episode(specials.id, 1, "S0E1", 30))
                .await
                .unwrap();
            let (other, _) = new_seasons(repo).await;
            let elsewhere = repo
                .find_or_create_episode(episode(other, 1, "other", 30))
                .await
                .unwrap();
            for episode_id in [s1e1.id, s2e1.id, elsewhere.id] {
                episode_file(&fixture, episode_id).await;
            }
            let gone = episode_file(&fixture, s0e1.id).await;
            fixture
                .files()
                .mark_missing(vec![gone.id], ::chrono::Utc::now())
                .await
                .unwrap();

            let outline: Vec<(Uuid, u32, u32, bool)> = repo
                .episode_outline(show.id)
                .await
                .unwrap()
                .into_iter()
                .map(|e| (e.episode_id, e.season_number, e.episode_number, e.playable))
                .collect();
            assert_eq!(
                outline,
                vec![
                    (s0e1.id, 0, 1, false),
                    (s1e1.id, 1, 1, true),
                    (s1e2.id, 1, 2, false),
                    (s2e1.id, 2, 1, true),
                ],
                "a missing file does not make an episode playable"
            );
        }

        /// Continue-watching reads a page of shows' outlines at once: each
        /// show's is its own outline, and a show with no episodes is absent.
        #[tokio::test]
        async fn outlines_of_several_shows_are_each_shows_own() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (a_one, a_two) = new_seasons(repo).await;
            let (b_one, _) = new_seasons(repo).await;
            let a_s2e1 = repo
                .find_or_create_episode(episode(a_two, 1, "A S2E1", 30))
                .await
                .unwrap();
            let a_s1e1 = repo
                .find_or_create_episode(episode(a_one, 1, "A S1E1", 30))
                .await
                .unwrap();
            let b_s1e1 = repo
                .find_or_create_episode(episode(b_one, 1, "B S1E1", 30))
                .await
                .unwrap();
            episode_file(&fixture, b_s1e1.id).await;
            let show_of = |season_id: Uuid| async move {
                repo.find_season_by_id(season_id)
                    .await
                    .unwrap()
                    .expect("the season")
                    .show_id
            };
            let (a, b) = (show_of(a_one).await, show_of(b_one).await);
            let empty = repo
                .find_or_create_by_identity(new_show("no episodes"))
                .await
                .unwrap()
                .id;

            let outlines = repo.episode_outlines(&[a, b, empty]).await.unwrap();
            assert_eq!(outlines.len(), 2, "a show with no episodes is absent");
            for show in [a, b] {
                assert_eq!(
                    outlines[&show],
                    repo.episode_outline(show).await.unwrap(),
                    "each show's outline is its own"
                );
            }
            let ids: Vec<(Uuid, bool)> = outlines[&a]
                .iter()
                .map(|e| (e.episode_id, e.playable))
                .collect();
            assert_eq!(ids, vec![(a_s1e1.id, false), (a_s2e1.id, false)]);
            assert_eq!(
                outlines[&b].iter().map(|e| e.playable).collect::<Vec<_>>(),
                vec![true]
            );
            assert!(repo.episode_outlines(&[]).await.unwrap().is_empty());
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
                &$crate::models::enrichment::FieldLocks::none(),
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
                &$crate::models::enrichment::FieldLocks::none(),
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

        #[tokio::test]
        async fn a_pin_finds_the_show_pinned_to_it_before_one_matched_to_its_id() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let pin = ProviderPin::Tmdb(603);
            assert!(repo.find_by_pin(&pin).await.unwrap().is_none());

            let matched = repo
                .find_or_create_by_identity(new_show("Matched"))
                .await
                .unwrap();
            repo.apply_enrichment(
                matched.id,
                &ShowEnrichment {
                    title: "Matched".to_string(),
                    tmdb_id: Some(603),
                    imdb_id: Some("tt0133093".to_string()),
                    anilist_id: Some(5114),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            for by_id in [
                ProviderPin::Tmdb(603),
                ProviderPin::Imdb("tt0133093".to_string()),
                ProviderPin::Anilist(5114),
            ] {
                assert_eq!(
                    repo.find_by_pin(&by_id).await.unwrap().map(|t| t.id),
                    Some(matched.id),
                    "the show enrichment matched to {by_id}"
                );
            }

            let pinned = repo
                .find_or_create_by_identity(new_show("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(pinned.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_pin(&pin).await.unwrap().map(|t| t.id),
                Some(pinned.id),
                "a pin before a match"
            );
            assert_eq!(
                repo.find_by_id(pinned.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:603")
            );
            assert!(
                repo.find_by_pin(&ProviderPin::Tvdb(81189))
                    .await
                    .unwrap()
                    .is_none(),
                "an id nothing carries finds nothing"
            );
        }

        #[tokio::test]
        async fn one_pin_pins_one_show_and_a_show_can_be_repinned() {
            use $crate::models::pin::{PinSource, ProviderPin};
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
            let pin = ProviderPin::Anilist(5114);

            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "another show holds the pin"
            );
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().pinned_ref,
                None
            );
            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "pinning a show to its own pin again is no clash"
            );

            assert!(
                repo.set_pinned_ref(a.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_id(a.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:1"),
                "a new pin replaces the old"
            );
            assert!(
                repo.set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "a released pin is free"
            );
            assert!(
                !repo
                    .set_pinned_ref(Uuid::new_v4(), &ProviderPin::Tvdb(7), PinSource::Nfo)
                    .await
                    .unwrap(),
                "no show, no pin"
            );
        }

        #[tokio::test]
        async fn an_nfo_pin_never_replaces_an_administrators_pin_of_a_show() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_show("Pinned"))
                .await
                .unwrap();
            let stored = |title: Option<_>| {
                title.map(|t: $crate::models::Show| (t.pinned_ref, t.pin_source))
            };

            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:1".to_string()), Some(PinSource::Nfo))),
                "the pin is recorded with who set it"
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(2), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces an NFO's pin"
            );
            assert!(
                !repo
                    .set_pinned_ref(title.id, &ProviderPin::Tmdb(3), PinSource::Nfo)
                    .await
                    .unwrap(),
                "an NFO never replaces an administrator's pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:2".to_string()), Some(PinSource::Admin)))
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(4), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces their own pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:4".to_string()), Some(PinSource::Admin)))
            );
        }

        #[tokio::test]
        async fn enrichment_never_rewrites_a_shows_pin() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_show("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(
                    title.id,
                    &ProviderPin::Imdb("tt0113277".to_string()),
                    PinSource::Nfo
                )
                .await
                .unwrap()
            );
            repo.apply_enrichment(
                title.id,
                &ShowEnrichment {
                    title: "Provider".to_string(),
                    tmdb_id: Some(949),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            assert_eq!(
                repo.find_by_id(title.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("imdb:tt0113277")
            );
        }

        #[tokio::test]
        async fn enrichment_leaves_a_shows_locked_fields_and_writes_the_rest() {
            use $crate::models::enrichment::{FieldLocks, MetadataField};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_show("Locked"))
                .await
                .unwrap();
            let before = repo.find_by_id(title.id).await.unwrap().unwrap();
            let locks: FieldLocks = [
                MetadataField::Title,
                MetadataField::Year,
                MetadataField::Poster,
                MetadataField::Rating,
            ]
            .into_iter()
            .collect();
            repo.apply_enrichment(
                title.id,
                &ShowEnrichment {
                    tmdb_id: Some(1396),
                    imdb_id: None,
                    anilist_id: None,
                    title: "Breaking Bad".to_string(),
                    original_title: Some("Breaking Bad (orig)".to_string()),
                    description: Some("A teacher turns.".to_string()),
                    year: Some(2008),
                    poster_url: Some("https://img/p.jpg".to_string()),
                    backdrop_url: Some("https://img/b.jpg".to_string()),
                    rating: Some(9.5),
                    genres: Vec::new(),
                },
                &locks,
            )
            .await
            .unwrap();
            let after = repo.find_by_id(title.id).await.unwrap().unwrap();
            assert_eq!(after.title, before.title);
            assert_eq!(after.year, before.year);
            assert_eq!(after.poster_url, before.poster_url);
            assert_eq!(after.rating_tmdb, before.rating_tmdb);
            assert_eq!(
                after.title_localized.as_deref(),
                Some("Breaking Bad (orig)")
            );
            assert_eq!(after.description.as_deref(), Some("A teacher turns."));
            assert_eq!(after.backdrop_url.as_deref(), Some("https://img/b.jpg"));
            assert_eq!(after.tmdb_id, Some(1396), "the match is always written");
        }

        #[tokio::test]
        async fn only_an_administrators_show_pin_is_cleared() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let admin = repo
                .find_or_create_by_identity(new_show("Admin pinned"))
                .await
                .unwrap();
            let nfo = repo
                .find_or_create_by_identity(new_show("NFO pinned"))
                .await
                .unwrap();
            let admin_pin = ProviderPin::Tvdb(900_000 + (admin.id.as_u128() % 90_000) as u32);
            let nfo_pin = ProviderPin::Tvdb(800_000 + (nfo.id.as_u128() % 90_000) as u32);
            assert!(
                repo.set_pinned_ref(admin.id, &admin_pin, PinSource::Admin)
                    .await
                    .unwrap()
            );
            assert!(
                repo.set_pinned_ref(nfo.id, &nfo_pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );

            assert!(repo.clear_admin_pin(admin.id).await.unwrap());
            let cleared = repo.find_by_id(admin.id).await.unwrap().unwrap();
            assert_eq!((cleared.pinned_ref, cleared.pin_source), (None, None));
            assert!(!repo.clear_admin_pin(nfo.id).await.unwrap());
            let kept = repo.find_by_id(nfo.id).await.unwrap().unwrap();
            assert_eq!(kept.pin_source, Some(PinSource::Nfo));
            assert!(!repo.clear_admin_pin(Uuid::new_v4()).await.unwrap());
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
                        identity: None,
                        mime_type: Some("video/x-matroska".to_string()),
                        duration: None,
                        container_format: Some("matroska".to_string()),
                        content: Some(MediaFileContent::Movie {
                            movie_entry_id: entry.id,
                        }),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
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
                &$crate::models::enrichment::FieldLocks::none(),
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
                &$crate::models::enrichment::FieldLocks::none(),
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
                &$crate::models::enrichment::FieldLocks::none(),
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

        #[tokio::test]
        async fn a_pin_finds_the_movie_pinned_to_it_before_one_matched_to_its_id() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let pin = ProviderPin::Tmdb(603);
            assert!(repo.find_by_pin(&pin).await.unwrap().is_none());

            let matched = repo
                .find_or_create_by_identity(new_movie("Matched"))
                .await
                .unwrap();
            repo.apply_enrichment(
                matched.id,
                &MovieEnrichment {
                    title: "Matched".to_string(),
                    tmdb_id: Some(603),
                    imdb_id: Some("tt0133093".to_string()),
                    anilist_id: Some(5114),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            for by_id in [
                ProviderPin::Tmdb(603),
                ProviderPin::Imdb("tt0133093".to_string()),
                ProviderPin::Anilist(5114),
            ] {
                assert_eq!(
                    repo.find_by_pin(&by_id).await.unwrap().map(|t| t.id),
                    Some(matched.id),
                    "the movie enrichment matched to {by_id}"
                );
            }

            let pinned = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(pinned.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_pin(&pin).await.unwrap().map(|t| t.id),
                Some(pinned.id),
                "a pin before a match"
            );
            assert_eq!(
                repo.find_by_id(pinned.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:603")
            );
            assert!(
                repo.find_by_pin(&ProviderPin::Tvdb(81189))
                    .await
                    .unwrap()
                    .is_none(),
                "an id nothing carries finds nothing"
            );
        }

        #[tokio::test]
        async fn one_pin_pins_one_movie_and_a_movie_can_be_repinned() {
            use $crate::models::pin::{PinSource, ProviderPin};
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
            let pin = ProviderPin::Anilist(5114);

            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "another movie holds the pin"
            );
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().pinned_ref,
                None
            );
            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "pinning a movie to its own pin again is no clash"
            );

            assert!(
                repo.set_pinned_ref(a.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_id(a.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:1"),
                "a new pin replaces the old"
            );
            assert!(
                repo.set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "a released pin is free"
            );
            assert!(
                !repo
                    .set_pinned_ref(Uuid::new_v4(), &ProviderPin::Tvdb(7), PinSource::Nfo)
                    .await
                    .unwrap(),
                "no movie, no pin"
            );
        }

        #[tokio::test]
        async fn an_nfo_pin_never_replaces_an_administrators_pin_of_a_movie() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            let stored = |title: Option<_>| {
                title.map(|t: $crate::models::Movie| (t.pinned_ref, t.pin_source))
            };

            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:1".to_string()), Some(PinSource::Nfo))),
                "the pin is recorded with who set it"
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(2), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces an NFO's pin"
            );
            assert!(
                !repo
                    .set_pinned_ref(title.id, &ProviderPin::Tmdb(3), PinSource::Nfo)
                    .await
                    .unwrap(),
                "an NFO never replaces an administrator's pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:2".to_string()), Some(PinSource::Admin)))
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(4), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces their own pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:4".to_string()), Some(PinSource::Admin)))
            );
        }

        #[tokio::test]
        async fn enrichment_never_rewrites_a_movies_pin() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(
                    title.id,
                    &ProviderPin::Imdb("tt0113277".to_string()),
                    PinSource::Nfo
                )
                .await
                .unwrap()
            );
            repo.apply_enrichment(
                title.id,
                &MovieEnrichment {
                    title: "Provider".to_string(),
                    tmdb_id: Some(949),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            assert_eq!(
                repo.find_by_id(title.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("imdb:tt0113277")
            );
        }

        #[tokio::test]
        async fn enrichment_leaves_every_locked_field_and_writes_the_rest() {
            use $crate::models::enrichment::{FieldLocks, MetadataField};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let enrichment = MovieEnrichment {
                tmdb_id: Some(603),
                imdb_id: Some("tt0133093".to_string()),
                anilist_id: None,
                title: "The Matrix".to_string(),
                original_title: Some("Matrix".to_string()),
                description: Some("A hacker learns the truth.".to_string()),
                year: Some(1999),
                release_date: ::chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
                runtime_mins: Some(136),
                poster_url: Some("https://img/poster.jpg".to_string()),
                backdrop_url: Some("https://img/backdrop.jpg".to_string()),
                rating: Some(8.2),
                genres: Vec::new(),
            };
            // Each field locked alone: that one keeps the value it had before
            // enrichment, and every other takes the provider's.
            for field in MetadataField::ALL
                .into_iter()
                .filter(|f| *f != MetadataField::Genres)
            {
                let title = repo
                    .find_or_create_by_identity(parsed("Locked", Some(1998)))
                    .await
                    .unwrap();
                let before = repo.find_by_id(title.id).await.unwrap().unwrap();
                let locks: FieldLocks = [field].into_iter().collect();
                // A provider id matches one movie: each gets its own.
                let tmdb_id = 900_000 + field as u32;
                let enrichment = MovieEnrichment {
                    tmdb_id: Some(tmdb_id),
                    imdb_id: Some(format!("tt{:07}", tmdb_id)),
                    ..enrichment.clone()
                };
                repo.apply_enrichment(title.id, &enrichment, &locks)
                    .await
                    .unwrap();
                let after = repo.find_by_id(title.id).await.unwrap().unwrap();

                let pick = |m: &Movie| -> String {
                    match field {
                        MetadataField::Title => m.title.clone(),
                        MetadataField::OriginalTitle => format!("{:?}", m.title_localized),
                        MetadataField::Description => format!("{:?}", m.description),
                        MetadataField::Year => format!("{:?}", m.year),
                        MetadataField::ReleaseDate => format!("{:?}", m.release_date),
                        MetadataField::Runtime => format!("{:?}", m.runtime),
                        MetadataField::Poster => format!("{:?}", m.poster_url),
                        MetadataField::Backdrop => format!("{:?}", m.backdrop_url),
                        MetadataField::Rating => format!("{:?}", m.rating_tmdb),
                        MetadataField::Genres => unreachable!("genres are not a movie column"),
                    }
                };
                assert_eq!(pick(&after), pick(&before), "{field:?} is locked");
                let written = [
                    (MetadataField::Title, after.title == "The Matrix"),
                    (
                        MetadataField::OriginalTitle,
                        after.title_localized.as_deref() == Some("Matrix"),
                    ),
                    (
                        MetadataField::Description,
                        after.description.as_deref() == Some("A hacker learns the truth."),
                    ),
                    (MetadataField::Year, after.year == Some(1999)),
                    (
                        MetadataField::ReleaseDate,
                        after.release_date == ::chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
                    ),
                    (
                        MetadataField::Runtime,
                        after.runtime == Some(::std::time::Duration::from_secs(136 * 60)),
                    ),
                    (
                        MetadataField::Poster,
                        after.poster_url.as_deref() == Some("https://img/poster.jpg"),
                    ),
                    (
                        MetadataField::Backdrop,
                        after.backdrop_url.as_deref() == Some("https://img/backdrop.jpg"),
                    ),
                    (MetadataField::Rating, after.rating_tmdb == Some(8.2)),
                ];
                for (other, was_written) in written {
                    if other != field {
                        assert!(
                            was_written,
                            "{other:?} is written while {field:?} is locked"
                        );
                    }
                }
                assert_eq!(after.tmdb_id, Some(tmdb_id), "the match is always written");
                assert_eq!(after.imdb_id, enrichment.imdb_id);
            }
        }

        #[tokio::test]
        async fn only_an_administrators_pin_is_cleared() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let admin = repo
                .find_or_create_by_identity(new_movie("Admin pinned"))
                .await
                .unwrap();
            let nfo = repo
                .find_or_create_by_identity(new_movie("NFO pinned"))
                .await
                .unwrap();
            let bare = repo
                .find_or_create_by_identity(new_movie("Unpinned"))
                .await
                .unwrap();
            let admin_pin = ProviderPin::Tmdb(900_000 + (admin.id.as_u128() % 90_000) as u32);
            let nfo_pin = ProviderPin::Tmdb(800_000 + (nfo.id.as_u128() % 90_000) as u32);
            assert!(
                repo.set_pinned_ref(admin.id, &admin_pin, PinSource::Admin)
                    .await
                    .unwrap()
            );
            assert!(
                repo.set_pinned_ref(nfo.id, &nfo_pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );

            assert!(repo.clear_admin_pin(admin.id).await.unwrap());
            let cleared = repo.find_by_id(admin.id).await.unwrap().unwrap();
            assert_eq!((cleared.pinned_ref, cleared.pin_source), (None, None));
            assert!(
                !repo.clear_admin_pin(admin.id).await.unwrap(),
                "nothing left to clear"
            );
            assert!(!repo.clear_admin_pin(nfo.id).await.unwrap());
            let kept = repo.find_by_id(nfo.id).await.unwrap().unwrap();
            assert_eq!(
                (kept.pinned_ref, kept.pin_source),
                (Some(nfo_pin.to_ref_string()), Some(PinSource::Nfo)),
                "an NFO's pin is the NFO's to change"
            );
            assert!(!repo.clear_admin_pin(bare.id).await.unwrap());
            assert!(!repo.clear_admin_pin(Uuid::new_v4()).await.unwrap());
            assert!(
                repo.set_pinned_ref(nfo.id, &admin_pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "the cleared id is free for another title"
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
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: container.map(str::to_string),
                    content,
                    status,
                    classifier_version: 0,
                    container_tags: None,
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
                    is_hearing_impaired: false,
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

/// Behavioural contract for [`crate::repositories::SidecarSubtitleRepository`]
/// (issue #184): one row per subtitle path, upserted in place, listed by file
/// and by library in path order, and deleted by id.
///
/// `$setup` names an `async fn() -> impl SidecarSubtitleFixture`.
#[macro_export]
macro_rules! sidecar_subtitle_repository_contract {
    ($setup:path) => {
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
        use $crate::repositories::contract::fixture::SidecarSubtitleFixture;

        /// A fixed, non-epoch instant a whole second survives Postgres's
        /// microsecond precision at.
        fn at(offset_secs: i64) -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::DateTime::from_timestamp(1_700_000_000 + offset_secs, 0)
                .expect("valid instant")
        }

        /// A subtitle of `file_id` at a path of its own -- a fresh UUID keeps
        /// concurrently running Postgres tests apart -- named `name`.
        fn subtitle(
            library_id: Uuid,
            file_id: Uuid,
            dir: &str,
            name: &str,
        ) -> UpsertSidecarSubtitle {
            UpsertSidecarSubtitle {
                file_id,
                library_id,
                path: PathBuf::from(format!("/videos/{dir}/{name}")),
                info: SidecarInfo {
                    format: SubtitleFormat::Srt,
                    language: Some("eng".to_string()),
                    title: None,
                    is_forced: false,
                    is_sdh: false,
                    is_default: false,
                },
                size_bytes: 100,
                mtime: Some(at(0)),
            }
        }

        #[tokio::test]
        async fn an_upsert_inserts_once_then_updates_the_row_at_its_path_in_place() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let other_video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let first = subtitle(library, video, &dir, "Movie.en.srt");

            let inserted = repo.upsert_by_path(first.clone()).await.unwrap();
            assert!(first.matches(&inserted), "{inserted:?}");
            assert_eq!(
                repo.find_by_path(&first.path).await.unwrap().map(|r| r.id),
                Some(inserted.id)
            );

            let changed = UpsertSidecarSubtitle {
                file_id: other_video,
                info: SidecarInfo {
                    format: SubtitleFormat::Ass,
                    language: None,
                    title: Some("Commentary".to_string()),
                    is_forced: true,
                    is_sdh: true,
                    is_default: true,
                },
                size_bytes: 250,
                mtime: None,
                ..first.clone()
            };
            let updated = repo.upsert_by_path(changed.clone()).await.unwrap();
            assert_eq!(updated.id, inserted.id, "one row per path, kept in place");
            assert_eq!(updated.created_at, inserted.created_at);
            assert!(changed.matches(&updated), "{updated:?}");
            let stored = repo.find_by_path(&first.path).await.unwrap().unwrap();
            assert!(changed.matches(&stored), "{stored:?}");
            assert!(repo.find_by_file_id(video).await.unwrap().is_empty());
            assert_eq!(repo.find_by_file_id(other_video).await.unwrap().len(), 1);
        }

        /// `sidecar_subtitles.mtime` is a `TIMESTAMPTZ`, as `files.mtime` is:
        /// a filesystem's nanoseconds read back as whole microseconds
        /// (issue #229), which is what a scan compares a subtitle's stat with.
        #[tokio::test]
        async fn an_mtime_reads_back_in_whole_microseconds() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let written = ::chrono::DateTime::from_timestamp(1_790_000_341, 802_029_432)
                .expect("valid instant");
            let read_back = ::chrono::DateTime::from_timestamp(1_790_000_341, 802_029_000)
                .expect("valid instant");

            let upserted = repo
                .upsert_by_path(UpsertSidecarSubtitle {
                    mtime: Some(written),
                    ..subtitle(library, video, &dir, "Movie.en.srt")
                })
                .await
                .unwrap();
            assert_eq!(upserted.mtime, Some(read_back));
            let stored = repo.find_by_path(&upserted.path).await.unwrap().unwrap();
            assert_eq!(stored.mtime, Some(read_back));
        }

        #[tokio::test]
        async fn subtitles_are_listed_by_file_and_by_library_in_path_order() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let sibling = fixture.new_video_file(library).await;
            let elsewhere = fixture.new_video_file(other_library).await;
            let dir = Uuid::new_v4().to_string();
            for upsert in [
                subtitle(library, video, &dir, "Movie.fr.srt"),
                subtitle(library, video, &dir, "Movie.en.srt"),
                subtitle(library, sibling, &dir, "Other.en.srt"),
                subtitle(other_library, elsewhere, &dir, "Elsewhere.en.srt"),
            ] {
                repo.upsert_by_path(upsert).await.unwrap();
            }
            let names = |rows: Vec<$crate::models::sidecar::SidecarSubtitle>| {
                rows.into_iter()
                    .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                names(repo.find_by_file_id(video).await.unwrap()),
                vec!["Movie.en.srt", "Movie.fr.srt"]
            );
            assert_eq!(
                names(repo.find_all_by_library(library).await.unwrap()),
                vec!["Movie.en.srt", "Movie.fr.srt", "Other.en.srt"]
            );
            assert_eq!(
                names(repo.find_all_by_library(other_library).await.unwrap()),
                vec!["Elsewhere.en.srt"]
            );
        }

        #[tokio::test]
        async fn delete_by_ids_removes_exactly_those_rows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let keep = repo
                .upsert_by_path(subtitle(library, video, &dir, "Movie.en.srt"))
                .await
                .unwrap();
            let gone = repo
                .upsert_by_path(subtitle(library, video, &dir, "Movie.de.srt"))
                .await
                .unwrap();

            assert_eq!(repo.delete_by_ids(Vec::new()).await.unwrap(), 0);
            assert_eq!(
                repo.delete_by_ids(vec![gone.id, Uuid::new_v4()])
                    .await
                    .unwrap(),
                1,
                "an unknown id deletes nothing"
            );
            assert_eq!(repo.find_by_path(&gone.path).await.unwrap(), None);
            assert_eq!(
                repo.find_by_file_id(video)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![keep.id]
            );
        }

        #[tokio::test]
        async fn an_unknown_path_finds_nothing() {
            let fixture = $setup().await;
            let path = PathBuf::from(format!("/videos/{}/None.srt", Uuid::new_v4()));
            assert_eq!(fixture.repo().find_by_path(&path).await.unwrap(), None);
        }
    };
}

/// Behavioural contract for [`crate::repositories::AppliedNfoRepository`]
/// (issue #184): one record per NFO path, recorded in place, listed by
/// library in path order, and deleted by id or by the directory above it.
///
/// `$setup` names an `async fn() -> impl AppliedNfoFixture`.
#[macro_export]
macro_rules! applied_nfo_repository_contract {
    ($setup:path) => {
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};
        use $crate::repositories::contract::fixture::AppliedNfoFixture;

        /// The NFO `name` in a folder of its own -- a fresh UUID keeps
        /// concurrently running Postgres tests apart.
        fn nfo(library_id: Uuid, dir: &str, name: &str) -> RecordAppliedNfo {
            RecordAppliedNfo {
                library_id,
                path: PathBuf::from(format!("/videos/{dir}/{name}")),
                size_bytes: 120,
                content_hash: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string(),
                change_stamp: Some("120:1700000000000000000:1700000000000000000".to_string()),
            }
        }

        #[tokio::test]
        async fn a_record_inserts_once_then_updates_the_row_at_its_path_in_place() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            let first = nfo(library, &dir, "movie.nfo");

            let inserted = repo.record_by_path(first.clone()).await.unwrap();
            assert!(first.matches(&inserted), "{inserted:?}");
            assert_eq!(
                repo.find_by_path(&first.path).await.unwrap().map(|r| r.id),
                Some(inserted.id)
            );

            let edited = RecordAppliedNfo {
                size_bytes: 64,
                content_hash: "ffeeddccbbaa99887766554433221100".to_string(),
                change_stamp: None,
                ..first.clone()
            };
            assert!(!edited.same_content(&inserted));
            let updated = repo.record_by_path(edited.clone()).await.unwrap();
            assert_eq!(updated.id, inserted.id, "one row per path, kept in place");
            assert_eq!(updated.created_at, inserted.created_at);
            let stored = repo.find_by_path(&first.path).await.unwrap().unwrap();
            assert!(edited.matches(&stored), "{stored:?}");
            assert!(!first.same_content(&stored), "the edit is what is recorded");
        }

        #[tokio::test]
        async fn records_are_listed_by_library_in_path_order() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            for record in [
                nfo(library, &dir, "tvshow.nfo"),
                nfo(library, &dir, "movie.nfo"),
                nfo(other_library, &dir, "Elsewhere.nfo"),
            ] {
                repo.record_by_path(record).await.unwrap();
            }
            let names = |rows: Vec<AppliedNfo>| {
                rows.into_iter()
                    .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                names(repo.find_all_by_library(library).await.unwrap()),
                vec!["movie.nfo", "tvshow.nfo"]
            );
            assert_eq!(
                names(repo.find_all_by_library(other_library).await.unwrap()),
                vec!["Elsewhere.nfo"]
            );
        }

        #[tokio::test]
        async fn delete_by_ids_removes_exactly_those_rows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            let keep = repo
                .record_by_path(nfo(library, &dir, "movie.nfo"))
                .await
                .unwrap();
            let gone = repo
                .record_by_path(nfo(library, &dir, "tvshow.nfo"))
                .await
                .unwrap();

            assert_eq!(repo.delete_by_ids(Vec::new()).await.unwrap(), 0);
            assert_eq!(
                repo.delete_by_ids(vec![gone.id, Uuid::new_v4()])
                    .await
                    .unwrap(),
                1,
                "an unknown id deletes nothing"
            );
            assert_eq!(repo.find_by_path(&gone.path).await.unwrap(), None);
            assert_eq!(
                repo.find_all_by_library(library)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![keep.id]
            );
        }

        #[tokio::test]
        async fn an_unknown_path_finds_nothing() {
            let fixture = $setup().await;
            let path = PathBuf::from(format!("/videos/{}/movie.nfo", Uuid::new_v4()));
            assert_eq!(fixture.repo().find_by_path(&path).await.unwrap(), None);
        }

        /// The records beneath a directory are those whose path continues it
        /// by whole components, at any depth -- another library's never. A
        /// `_` or `%` in the directory's name is itself, not a wildcard.
        #[tokio::test]
        async fn delete_beneath_removes_the_records_beneath_a_directory_by_whole_components() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let base = Uuid::new_v4().to_string();
            let dir = format!("{base}/Show_%1");
            for gone in [
                nfo(library, &dir, "tvshow.nfo"),
                nfo(library, &format!("{dir}/Season 01"), "S01E01.nfo"),
            ] {
                repo.record_by_path(gone).await.unwrap();
            }
            // Longer by a character, what the wildcards would match, a
            // sibling file named like the directory, and another library.
            let kept = [
                nfo(library, &format!("{base}/Show_%10"), "tvshow.nfo"),
                nfo(library, &format!("{base}/ShowAB1"), "tvshow.nfo"),
                nfo(library, &base, "Show_%1.nfo"),
                nfo(other, &format!("{dir}/Season 02"), "S02E01.nfo"),
            ];
            for record in kept.clone() {
                repo.record_by_path(record).await.unwrap();
            }

            let dir_path = PathBuf::from(format!("/videos/{dir}"));
            assert_eq!(repo.delete_beneath(library, &dir_path).await.unwrap(), 2);

            let mut left: Vec<PathBuf> = repo
                .find_all_by_library(library)
                .await
                .unwrap()
                .into_iter()
                .chain(repo.find_all_by_library(other).await.unwrap())
                .map(|r| r.path)
                .collect();
            left.sort();
            let mut expected: Vec<PathBuf> = kept.into_iter().map(|r| r.path).collect();
            expected.sort();
            assert_eq!(left, expected);
            assert_eq!(
                repo.delete_beneath(library, &dir_path).await.unwrap(),
                0,
                "nothing is left beneath it"
            );
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
                    identity: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
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
                        &$crate::models::enrichment::FieldLocks::none(),
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
                        &$crate::models::enrichment::FieldLocks::none(),
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

            // A group is one key: its titles tie, and the id -- not the kind
            // -- orders them (strict order is the next test's to check).
            assert_grouped(
                &order_of(&fixture, &made, Year, Asc).await,
                &[&["charlie", "golf"], &["alpha", "Echo"], &["delta"], &["Bravo", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Year, Desc).await,
                &[&["delta"], &["alpha", "Echo"], &["charlie", "golf"], &["Bravo", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Asc).await,
                &[&["Echo", "golf"], &["alpha"], &["delta"], &["Bravo", "charlie", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Desc).await,
                &[&["delta"], &["alpha"], &["Echo", "golf"], &["Bravo", "charlie", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Asc).await,
                &[&["charlie"], &["alpha"], &["Bravo", "delta", "Echo", "Foxtrot", "golf"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Desc).await,
                &[&["alpha"], &["charlie"], &["Bravo", "delta", "Echo", "Foxtrot", "golf"]],
            );
            let created: Vec<&str> = made.iter().map(|(_, name)| *name).collect();
            assert_eq!(order_of(&fixture, &made, DateAdded, Asc).await, created);
            let mut newest_first = created.clone();
            newest_first.reverse();
            assert_eq!(order_of(&fixture, &made, DateAdded, Desc).await, newest_first);
        }

        /// Whatever the sort, every row carries the key its title really has,
        /// and each row sorts strictly after the one before it -- ties
        /// included, which only the `(id, kind)` tie-break can separate.
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

        /// The minimum rating compares in the precision a rating is stored
        /// and shown in: a 7.2 is 72 per cent, not 71.99..., for either kind.
        #[tokio::test]
        async fn a_rating_exactly_at_a_fractional_minimum_is_kept() {
            let fixture = $setup().await;
            let made = vec![
                (movie(&fixture, "lima", None, None, Some(7.2), true).await.id, "lima"),
                (show(&fixture, "mike", None, Some(7.2), 1, true).await.0, "mike"),
                (movie(&fixture, "november", None, None, Some(7.1), true).await.id, "november"),
            ];
            assert_eq!(
                listed_names(
                    &fixture,
                    &made,
                    CatalogFilters { min_rating: Some(72), ..Default::default() },
                )
                .await,
                ["lima", "mike"]
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

/// Behavioural contract for [`crate::repositories::EnrichmentStateRepository`]'s
/// administrator surface (issue #185): a title's row read by its title, locks
/// that replace whole and survive every status change, refreshes of a library's
/// titles or of all of them, and the admin list's filters, order and pages.
///
/// `$setup` names an `async fn() -> impl EnrichmentStateFixture`.
#[macro_export]
macro_rules! enrichment_state_repository_contract {
    ($setup:path) => {
        use ::chrono::{DateTime, TimeZone, Utc};
        use ::std::num::NonZeroU32;
        use ::uuid::Uuid;
        use $crate::models::catalog::TitleKind;
        use $crate::models::enrichment::{
            EnrichmentListFilter, EnrichmentListQuery, EnrichmentStatus, EnrichmentTargetId,
            FieldLocks, MetadataField,
        };
        use $crate::models::movie::CreateMovie;
        use $crate::models::show::CreateShow;
        use $crate::repositories::contract::fixture::EnrichmentStateFixture;

        async fn new_movie(fixture: &impl EnrichmentStateFixture) -> EnrichmentTargetId {
            let movie = fixture
                .movies()
                .find_or_create_by_identity(CreateMovie::new(
                    format!("enrichment movie {}", Uuid::new_v4()),
                    None,
                    None,
                ))
                .await
                .unwrap();
            EnrichmentTargetId::Movie(movie.id)
        }

        async fn new_show(fixture: &impl EnrichmentStateFixture) -> EnrichmentTargetId {
            let show = fixture
                .shows()
                .find_or_create_by_identity(CreateShow::new(
                    format!("enrichment show {}", Uuid::new_v4()),
                    None,
                ))
                .await
                .unwrap();
            EnrichmentTargetId::Show(show.id)
        }

        /// A title with a `Pending` row, returning the row's id.
        async fn queued(fixture: &impl EnrichmentStateFixture, target: EnrichmentTargetId) -> Uuid {
            let repo = fixture.repo();
            repo.ensure_pending(target).await.unwrap();
            repo.find_by_target(target).await.unwrap().unwrap().id
        }

        /// A whole second, so no store rounds it.
        fn at(secs: i64) -> DateTime<Utc> {
            Utc.timestamp_opt(1_900_000_000 + secs, 0).unwrap()
        }

        fn page(filter: EnrichmentListFilter, limit: u32) -> EnrichmentListQuery {
            EnrichmentListQuery {
                filter,
                after: None,
                limit: NonZeroU32::new(limit).unwrap(),
            }
        }

        fn locks(fields: &[MetadataField]) -> FieldLocks {
            fields.iter().copied().collect()
        }

        #[tokio::test]
        async fn a_row_is_found_by_its_title_and_only_by_its_title() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let show = new_show(&fixture).await;
            let id = queued(&fixture, movie).await;

            let found = repo.find_by_target(movie).await.unwrap().unwrap();
            assert_eq!((found.id, found.target), (id, movie));
            assert_eq!(found.status, EnrichmentStatus::Pending);
            assert!(
                found.locked_fields.is_empty(),
                "nothing is locked by default"
            );
            assert!(repo.find_by_target(show).await.unwrap().is_none());
            assert!(
                repo.find_by_target(EnrichmentTargetId::Show(movie.id()))
                    .await
                    .unwrap()
                    .is_none(),
                "a movie's id does not find it as a show"
            );
        }

        #[tokio::test]
        async fn locks_replace_whole_and_leave_the_status_and_the_match_alone() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let id = queued(&fixture, movie).await;
            repo.mark_enriched(id, "tmdb:603", 0.9, at(0))
                .await
                .unwrap();

            let set = repo
                .set_locked_fields(
                    movie,
                    &locks(&[MetadataField::Title, MetadataField::Poster]),
                )
                .await
                .unwrap();
            assert_eq!(set.id, id, "the existing row is updated, not a second made");
            assert_eq!(
                set.locked_fields,
                locks(&[MetadataField::Title, MetadataField::Poster])
            );
            let replaced = repo
                .set_locked_fields(movie, &locks(&[MetadataField::Genres]))
                .await
                .unwrap();
            assert_eq!(replaced.locked_fields, locks(&[MetadataField::Genres]));

            let stored = repo.find_by_target(movie).await.unwrap().unwrap();
            assert_eq!(stored.locked_fields, locks(&[MetadataField::Genres]));
            assert_eq!(stored.status, EnrichmentStatus::Enriched);
            assert_eq!(stored.matched_ref.as_deref(), Some("tmdb:603"));

            // Every later status change keeps the locks.
            repo.request_refresh(movie, true).await.unwrap();
            repo.mark_unmatched(id, "gone", at(1)).await.unwrap();
            repo.mark_failed(id, "worse", at(2)).await.unwrap();
            repo.mark_retrying(id, "again", 1, at(3)).await.unwrap();
            repo.mark_enriched(id, "tmdb:604", 1.0, at(4))
                .await
                .unwrap();
            assert_eq!(
                repo.find_by_target(movie)
                    .await
                    .unwrap()
                    .unwrap()
                    .locked_fields,
                locks(&[MetadataField::Genres])
            );

            let cleared = repo
                .set_locked_fields(movie, &FieldLocks::none())
                .await
                .unwrap();
            assert!(cleared.locked_fields.is_empty());
        }

        #[tokio::test]
        async fn locking_a_title_with_no_row_gives_it_a_pending_one() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let show = new_show(&fixture).await;

            let set = repo
                .set_locked_fields(show, &locks(&[MetadataField::Description]))
                .await
                .unwrap();
            assert_eq!(set.target, show);
            assert_eq!(set.status, EnrichmentStatus::Pending);
            let stored = repo.find_by_target(show).await.unwrap().unwrap();
            assert_eq!(stored.id, set.id);
            assert_eq!(stored.locked_fields, locks(&[MetadataField::Description]));
            repo.ensure_pending(show).await.unwrap();
            assert_eq!(
                repo.count(&EnrichmentListFilter::default()).await.unwrap(),
                1,
                "one row per title"
            );
        }

        #[tokio::test]
        async fn every_field_can_be_locked_and_reads_back() {
            let fixture = $setup().await;
            let movie = new_movie(&fixture).await;
            let all: FieldLocks = MetadataField::ALL.into_iter().collect();
            let set = fixture.repo().set_locked_fields(movie, &all).await.unwrap();
            assert_eq!(set.locked_fields, all);
            assert_eq!(
                fixture
                    .repo()
                    .find_by_target(movie)
                    .await
                    .unwrap()
                    .unwrap()
                    .locked_fields,
                all
            );
        }

        #[tokio::test]
        async fn refreshing_a_library_queues_all_its_titles_and_no_other() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let movie = new_movie(&fixture).await;
            let show = new_show(&fixture).await;
            let rowless = new_movie(&fixture).await;
            let elsewhere = new_movie(&fixture).await;
            for target in [movie, show, elsewhere] {
                let id = queued(&fixture, target).await;
                repo.mark_enriched(id, "tmdb:1", 0.8, at(0)).await.unwrap();
            }
            for (library, target) in [
                (library, movie),
                (library, show),
                (library, rowless),
                (other, elsewhere),
            ] {
                match target {
                    EnrichmentTargetId::Movie(id) => fixture
                        .movies()
                        .ensure_library_association(library, id)
                        .await
                        .unwrap(),
                    EnrichmentTargetId::Show(id) => fixture
                        .shows()
                        .ensure_library_association(library, id)
                        .await
                        .unwrap(),
                }
            }

            assert_eq!(
                repo.request_refresh_library(library, false).await.unwrap(),
                3,
                "every title of the library, the one with no row included"
            );
            for target in [movie, show] {
                let row = repo.find_by_target(target).await.unwrap().unwrap();
                assert_eq!(row.status, EnrichmentStatus::Pending);
                assert!(row.force_refresh);
                assert_eq!(row.attempts, 0);
                assert_eq!(row.matched_ref.as_deref(), Some("tmdb:1"), "no rematch");
            }
            let made = repo.find_by_target(rowless).await.unwrap().unwrap();
            assert_eq!(made.status, EnrichmentStatus::Pending);
            assert_eq!(made.next_attempt_at, None, "due at once");
            let untouched = repo.find_by_target(elsewhere).await.unwrap().unwrap();
            assert_eq!(untouched.status, EnrichmentStatus::Enriched);

            assert_eq!(
                repo.request_refresh_library(library, true).await.unwrap(),
                3,
                "a title's row is queued, never made twice"
            );
            assert_eq!(
                repo.count(&EnrichmentListFilter::default()).await.unwrap(),
                4
            );
            assert!(
                repo.find_by_target(movie)
                    .await
                    .unwrap()
                    .unwrap()
                    .matched_ref
                    .is_none(),
                "a rematch clears the match"
            );
            assert_eq!(
                repo.request_refresh_library(fixture.new_library().await, false)
                    .await
                    .unwrap(),
                0
            );
        }

        #[tokio::test]
        async fn refreshing_all_queues_every_row() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let show = new_show(&fixture).await;
            let movie_row = queued(&fixture, movie).await;
            let show_row = queued(&fixture, show).await;
            repo.mark_enriched(movie_row, "tmdb:1", 0.8, at(0))
                .await
                .unwrap();
            repo.mark_failed(show_row, "boom", at(0)).await.unwrap();

            assert_eq!(repo.request_refresh_all(false).await.unwrap(), 2);
            for target in [movie, show] {
                let row = repo.find_by_target(target).await.unwrap().unwrap();
                assert_eq!(row.status, EnrichmentStatus::Pending, "{target:?}");
                assert!(row.force_refresh);
            }
            assert_eq!(
                repo.find_by_target(movie)
                    .await
                    .unwrap()
                    .unwrap()
                    .matched_ref
                    .as_deref(),
                Some("tmdb:1")
            );
            assert_eq!(repo.request_refresh_all(true).await.unwrap(), 2);
            assert!(
                repo.find_by_target(movie)
                    .await
                    .unwrap()
                    .unwrap()
                    .matched_ref
                    .is_none()
            );
        }

        #[tokio::test]
        async fn the_list_filters_by_status_and_kind_newest_change_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let older = new_movie(&fixture).await;
            let newer = new_movie(&fixture).await;
            let show = new_show(&fixture).await;
            let failed = new_movie(&fixture).await;
            let older_row = queued(&fixture, older).await;
            let newer_row = queued(&fixture, newer).await;
            let show_row = queued(&fixture, show).await;
            let failed_row = queued(&fixture, failed).await;
            repo.mark_unmatched(older_row, "no match", at(10))
                .await
                .unwrap();
            repo.mark_unmatched(newer_row, "no match", at(30))
                .await
                .unwrap();
            repo.mark_unmatched(show_row, "no match", at(20))
                .await
                .unwrap();
            repo.mark_failed(failed_row, "boom", at(40)).await.unwrap();

            let unmatched = EnrichmentListFilter {
                status: Some(EnrichmentStatus::Unmatched),
                kind: None,
            };
            let ids: Vec<Uuid> = repo
                .list(&page(unmatched, 10))
                .await
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect();
            assert_eq!(ids, vec![newer_row, show_row, older_row]);
            assert_eq!(repo.count(&unmatched).await.unwrap(), 3);

            let unmatched_movies = EnrichmentListFilter {
                status: Some(EnrichmentStatus::Unmatched),
                kind: Some(TitleKind::Movie),
            };
            let movies = repo.list(&page(unmatched_movies, 10)).await.unwrap();
            assert_eq!(
                movies.iter().map(|r| r.id).collect::<Vec<_>>(),
                vec![newer_row, older_row]
            );
            assert_eq!(repo.count(&unmatched_movies).await.unwrap(), 2);
            assert_eq!(movies[0].last_error.as_deref(), Some("no match"));
            assert_eq!(movies[0].updated_at, at(30));

            let shows = EnrichmentListFilter {
                status: None,
                kind: Some(TitleKind::Show),
            };
            assert_eq!(
                repo.list(&page(shows, 10))
                    .await
                    .unwrap()
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![show_row]
            );
            let everything = repo
                .list(&page(EnrichmentListFilter::default(), 3))
                .await
                .unwrap();
            assert_eq!(
                everything.iter().map(|r| r.id).collect::<Vec<_>>(),
                vec![failed_row, newer_row, show_row],
                "at most the limit, newest first"
            );
            assert_eq!(
                repo.count(&EnrichmentListFilter::default()).await.unwrap(),
                4
            );
        }

        #[tokio::test]
        async fn pages_follow_one_another_without_gaps_or_repeats() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let mut expected = Vec::new();
            // Two share a timestamp: the id orders them, so paging between
            // them neither skips nor repeats one.
            for secs in [5, 5, 3, 9, 1] {
                let target = new_movie(&fixture).await;
                let id = queued(&fixture, target).await;
                repo.mark_unmatched(id, "no match", at(secs)).await.unwrap();
                expected.push((at(secs), id));
            }
            expected.sort_by(|a, b| b.cmp(a));
            let expected: Vec<Uuid> = expected.into_iter().map(|(_, id)| id).collect();

            let mut seen = Vec::new();
            let mut after = None;
            loop {
                let rows = repo
                    .list(&EnrichmentListQuery {
                        filter: EnrichmentListFilter::default(),
                        after,
                        limit: NonZeroU32::new(2).unwrap(),
                    })
                    .await
                    .unwrap();
                assert!(rows.len() <= 2, "a page holds at most its limit");
                let Some(last) = rows.last() else {
                    break;
                };
                after = Some(last.list_position());
                seen.extend(rows.iter().map(|r| r.id));
            }
            assert_eq!(seen, expected);
        }
    };
}
