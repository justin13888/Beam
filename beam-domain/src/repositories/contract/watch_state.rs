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
                finishes_title: true,
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

            assert_eq!(
                repo.carry(kept, to).await.unwrap_err().to_string(),
                $crate::repositories::watch_state::carry_kind_mismatch().to_string(),
                "a movie is no episode"
            );
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
