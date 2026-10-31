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
            repo.mark_retrying(id, "again", 1, at(5), at(3))
                .await
                .unwrap();
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

        /// Issue #347: a limited sweep takes the same rows from every store.
        /// Never-attempted rows come first -- a newly indexed title is not
        /// starved behind a retry queue -- then the earliest retry, the id
        /// settling a tie; a retry not yet due, and a row that is no longer
        /// `Pending`, is never taken.
        #[tokio::test]
        async fn due_rows_are_taken_never_attempted_first_then_oldest_retry_up_to_the_limit() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let now = at(100);

            let mut fresh = Vec::new();
            for _ in 0..2 {
                let movie = new_movie(&fixture).await;
                fresh.push(queued(&fixture, movie).await);
            }
            let show = new_show(&fixture).await;
            fresh.push(queued(&fixture, show).await);
            fresh.sort();

            // Queued in the reverse of the order they are due, so neither
            // insertion order nor id order can pass for the retry order.
            let later_retry = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_retrying(later_retry, "busy", 2, now, at(0))
                .await
                .unwrap();
            let earlier_retry = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_retrying(earlier_retry, "busy", 1, at(10), at(0))
                .await
                .unwrap();

            let not_yet = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_retrying(not_yet, "busy", 1, at(101), at(0))
                .await
                .unwrap();
            let failed = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_failed(failed, "gave up", at(0)).await.unwrap();
            let enriched = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_enriched(enriched, "tmdb:603", 0.9, at(0))
                .await
                .unwrap();
            let unmatched = queued(&fixture, new_movie(&fixture).await).await;
            repo.mark_unmatched(unmatched, "no hit", at(0))
                .await
                .unwrap();

            let ids = |rows: Vec<$crate::models::enrichment::EnrichmentState>| {
                rows.into_iter().map(|row| row.id).collect::<Vec<Uuid>>()
            };
            let mut every_due = fresh.clone();
            every_due.extend([earlier_retry, later_retry]);
            assert_eq!(
                ids(repo.fetch_due(now, 50).await.unwrap()),
                every_due,
                "never attempted by id, then the earliest retry; a retry due \
                 exactly now is due"
            );
            assert_eq!(
                ids(repo.fetch_due(now, 4).await.unwrap()),
                every_due[..4],
                "the limit cuts the same order, never-attempted rows first"
            );
            assert_eq!(
                ids(repo.fetch_due(now, 2).await.unwrap()),
                fresh[..2],
                "a limit below the never-attempted count takes the lowest ids"
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
        async fn count_by_status_counts_rows_per_status() {
            use $crate::models::enrichment::EnrichmentStatusCounts;

            let fixture = $setup().await;
            let repo = fixture.repo();
            let empty = repo.count_by_status().await.unwrap();
            assert_eq!(
                (empty.pending, empty.enriched, empty.unmatched, empty.failed),
                (0, 0, 0, 0)
            );

            // Distinct counts per status, so a column read from the wrong
            // status cannot pass: one pending, two enriched, three unmatched,
            // four failed.
            let mut rows = Vec::new();
            for _ in 0..10 {
                let movie = new_movie(&fixture).await;
                rows.push(queued(&fixture, movie).await);
            }
            for id in &rows[1..3] {
                repo.mark_enriched(*id, "tmdb:1", 0.9, at(0)).await.unwrap();
            }
            for id in &rows[3..6] {
                repo.mark_unmatched(*id, "no match", at(0)).await.unwrap();
            }
            for id in &rows[6..10] {
                repo.mark_failed(*id, "boom", at(0)).await.unwrap();
            }
            // A retry stays pending: it is not a status of its own.
            repo.mark_retrying(rows[0], "transient", 1, at(60), at(0))
                .await
                .unwrap();

            let EnrichmentStatusCounts {
                pending,
                enriched,
                unmatched,
                failed,
            } = repo.count_by_status().await.unwrap();
            assert_eq!((pending, enriched, unmatched, failed), (1, 2, 3, 4));
        }

        /// A retry is a status change like any other: it is stamped at the
        /// `now` the caller passes, never the store's own clock, and the next
        /// attempt it schedules is the one it was given.
        #[tokio::test]
        async fn a_retry_is_stamped_at_the_given_now_and_schedules_the_given_attempt() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let id = queued(&fixture, movie).await;
            repo.mark_unmatched(id, "earlier", at(10)).await.unwrap();

            repo.mark_retrying(id, "busy", 3, at(500), at(42))
                .await
                .unwrap();

            let row = repo.find_by_target(movie).await.unwrap().unwrap();
            assert_eq!(row.status, EnrichmentStatus::Pending);
            assert_eq!(row.attempts, 3);
            assert_eq!(row.next_attempt_at, Some(at(500)));
            assert_eq!(row.last_error.as_deref(), Some("busy"));
            assert_eq!(row.updated_at, at(42), "stamped at the caller's now");
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
