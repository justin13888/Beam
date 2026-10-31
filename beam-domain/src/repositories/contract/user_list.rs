/// Behavioural contract for [`crate::repositories::UserListRepository`]: a
/// title is on a list once, lists and users are kept apart, a list reads
/// newest addition first a page at a time, and a title merge carries its
/// entries (issue #262).
///
/// `$setup` names an `async fn() -> impl UserListFixture`.
#[macro_export]
macro_rules! user_list_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use $crate::models::user_list::{UserList, UserListEntry, UserListPosition};
        use $crate::models::watch_state::TitleRef;
        use $crate::repositories::contract::fixture::UserListFixture as _;
        use $crate::services::Clock as _;

        fn titles(entries: &[UserListEntry]) -> Vec<TitleRef> {
            entries.iter().map(|e| e.title).collect()
        }

        /// A policy nobody restricted.
        fn open() -> $crate::models::access::AccessPolicy {
            $crate::models::access::AccessPolicy::unrestricted()
        }

        /// A page and the count leave out the titles a policy hides, so a
        /// page holds `limit` visible entries however many hidden ones fall
        /// between them (issue #328).
        #[tokio::test]
        async fn a_page_and_its_count_hold_only_the_titles_the_policy_admits() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let (kids, grown) = (fixture.new_library().await, fixture.new_library().await);
            let scenario = $crate::repositories::contract::fixture::AccessScenario::build(
                fixture.movies(),
                fixture.shows(),
                kids,
                grown,
            )
            .await;
            // Newest first: a hidden entry between every two visible ones.
            for title in [
                TitleRef::Movie(scenario.kids_movie),
                TitleRef::Movie(scenario.grown_movie),
                TitleRef::Show(scenario.kids_show),
                TitleRef::Show(scenario.grown_show),
            ] {
                repo.add(user, UserList::Watchlist, title).await.unwrap();
                fixture.clock().advance(Duration::from_secs(1));
            }
            let visible = [
                TitleRef::Show(scenario.kids_show),
                TitleRef::Movie(scenario.kids_movie),
            ];

            for policy in [
                scenario.kids_only(),
                scenario.both_under_parental_guidance(),
            ] {
                let first = repo
                    .find_page(user, UserList::Watchlist, &policy, None, 1)
                    .await
                    .unwrap();
                assert_eq!(titles(&first), visible[..1], "{policy:?}");
                let after = first.last().map(UserListPosition::from);
                let second = repo
                    .find_page(user, UserList::Watchlist, &policy, after, 1)
                    .await
                    .unwrap();
                assert_eq!(titles(&second), visible[1..], "{policy:?}");
                let after = second.last().map(UserListPosition::from);
                assert!(
                    repo.find_page(user, UserList::Watchlist, &policy, after, 1)
                        .await
                        .unwrap()
                        .is_empty(),
                    "{policy:?}"
                );
                assert_eq!(
                    repo.count(user, UserList::Watchlist, &policy)
                        .await
                        .unwrap(),
                    2,
                    "{policy:?}"
                );
            }

            let nothing = scenario.grown_under_teen();
            assert!(
                repo.find_page(user, UserList::Watchlist, &nothing, None, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                repo.count(user, UserList::Watchlist, &nothing)
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(
                repo.find_page(user, UserList::Watchlist, &open(), None, 10)
                    .await
                    .unwrap()
                    .len(),
                4,
                "a hidden title's entry is kept, not removed"
            );
            assert_eq!(
                repo.count(user, UserList::Watchlist, &open())
                    .await
                    .unwrap(),
                4
            );
        }

        #[tokio::test]
        async fn adding_a_title_twice_keeps_its_first_entry() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = TitleRef::Movie(fixture.new_movie().await);

            let first = repo.add(user, UserList::Watchlist, movie).await.unwrap();
            assert_eq!(first.user_id, user);
            assert_eq!(first.list, UserList::Watchlist);
            assert_eq!(first.title, movie);
            assert_eq!(first.added_at, fixture.clock().now());
            fixture.clock().advance(Duration::from_secs(60));
            let again = repo.add(user, UserList::Watchlist, movie).await.unwrap();

            assert_eq!(again, first, "the same entry, added when it first was");
            assert_eq!(
                repo.count(user, UserList::Watchlist, &open())
                    .await
                    .unwrap(),
                1
            );
        }

        #[tokio::test]
        async fn a_list_reads_newest_addition_first_a_page_at_a_time() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let mut added = Vec::new();
            for i in 0..5 {
                let title = if i % 2 == 0 {
                    TitleRef::Movie(fixture.new_movie().await)
                } else {
                    TitleRef::Show(fixture.new_show().await)
                };
                repo.add(user, UserList::Favourites, title).await.unwrap();
                added.push(title);
                fixture.clock().advance(Duration::from_secs(1));
            }
            added.reverse();

            let first = repo
                .find_page(user, UserList::Favourites, &open(), None, 2)
                .await
                .unwrap();
            assert_eq!(titles(&first), added[..2]);
            let after = first.last().map(UserListPosition::from);
            let second = repo
                .find_page(user, UserList::Favourites, &open(), after, 2)
                .await
                .unwrap();
            assert_eq!(titles(&second), added[2..4]);
            let after = second.last().map(UserListPosition::from);
            let third = repo
                .find_page(user, UserList::Favourites, &open(), after, 2)
                .await
                .unwrap();
            assert_eq!(titles(&third), added[4..]);
            assert_eq!(
                repo.count(user, UserList::Favourites, &open())
                    .await
                    .unwrap(),
                5
            );
        }

        #[tokio::test]
        async fn titles_added_in_one_instant_page_by_id_without_skipping_any() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let mut expected = Vec::new();
            for _ in 0..4 {
                let title = TitleRef::Movie(fixture.new_movie().await);
                expected.push(repo.add(user, UserList::Watchlist, title).await.unwrap());
            }
            // Newest first is `(added_at, id)` descending; every `added_at`
            // is the same, so the id decides.
            expected.sort_by(|a, b| b.id.cmp(&a.id));

            let mut seen = Vec::new();
            let mut after = None;
            loop {
                let page = repo
                    .find_page(user, UserList::Watchlist, &open(), after, 1)
                    .await
                    .unwrap();
                let Some(last) = page.last() else {
                    break;
                };
                after = Some(UserListPosition::from(last));
                seen.extend(page);
            }
            assert_eq!(seen, expected);
        }

        #[tokio::test]
        async fn lists_and_users_are_kept_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (alice, bob) = (fixture.new_user().await, fixture.new_user().await);
            let movie = TitleRef::Movie(fixture.new_movie().await);
            let series = TitleRef::Show(fixture.new_show().await);

            repo.add(alice, UserList::Watchlist, movie).await.unwrap();
            repo.add(alice, UserList::Favourites, series).await.unwrap();
            repo.add(bob, UserList::Favourites, movie).await.unwrap();

            let page = |user, list| async move {
                titles(&repo.find_page(user, list, &open(), None, 10).await.unwrap())
            };
            assert_eq!(page(alice, UserList::Watchlist).await, [movie]);
            assert_eq!(page(alice, UserList::Favourites).await, [series]);
            assert_eq!(page(bob, UserList::Favourites).await, [movie]);
            assert!(page(bob, UserList::Watchlist).await.is_empty());
            assert_eq!(
                repo.count(bob, UserList::Watchlist, &open()).await.unwrap(),
                0
            );
        }

        #[tokio::test]
        async fn a_title_reports_each_list_it_is_on() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = TitleRef::Movie(fixture.new_movie().await);
            let series = TitleRef::Show(fixture.new_show().await);

            assert!(repo.lists_of(user, movie).await.unwrap().is_empty());
            repo.add(user, UserList::Favourites, movie).await.unwrap();
            repo.add(user, UserList::Watchlist, movie).await.unwrap();
            repo.add(user, UserList::Watchlist, series).await.unwrap();

            assert_eq!(
                repo.lists_of(user, movie).await.unwrap(),
                [UserList::Watchlist, UserList::Favourites]
            );
            assert_eq!(
                repo.lists_of(user, series).await.unwrap(),
                [UserList::Watchlist]
            );
            let stranger = fixture.new_user().await;
            assert!(repo.lists_of(stranger, movie).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn removing_takes_the_title_off_that_list_only() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = TitleRef::Movie(fixture.new_movie().await);
            repo.add(user, UserList::Watchlist, movie).await.unwrap();
            repo.add(user, UserList::Favourites, movie).await.unwrap();

            assert!(repo.remove(user, UserList::Watchlist, movie).await.unwrap());
            assert!(
                !repo.remove(user, UserList::Watchlist, movie).await.unwrap(),
                "a title no longer on the list is no change"
            );
            assert_eq!(
                repo.lists_of(user, movie).await.unwrap(),
                [UserList::Favourites]
            );
            assert_eq!(
                repo.count(user, UserList::Watchlist, &open())
                    .await
                    .unwrap(),
                0
            );
        }

        #[tokio::test]
        async fn a_merge_carries_entries_and_keeps_the_earlier_addition() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (alice, bob) = (fixture.new_user().await, fixture.new_user().await);
            let loser = TitleRef::Movie(fixture.new_movie().await);
            let survivor = TitleRef::Movie(fixture.new_movie().await);

            let alices_first = repo.add(alice, UserList::Watchlist, loser).await.unwrap();
            fixture.clock().advance(Duration::from_secs(10));
            repo.add(alice, UserList::Watchlist, survivor)
                .await
                .unwrap();
            repo.add(bob, UserList::Favourites, loser).await.unwrap();

            repo.carry(loser, survivor).await.unwrap();

            assert!(repo.lists_of(alice, loser).await.unwrap().is_empty());
            assert!(repo.lists_of(bob, loser).await.unwrap().is_empty());
            let alices = repo
                .find_page(alice, UserList::Watchlist, &open(), None, 10)
                .await
                .unwrap();
            assert_eq!(titles(&alices), [survivor], "one entry, not two");
            assert_eq!(
                alices[0].added_at, alices_first.added_at,
                "added when the earlier of the two was"
            );
            assert_eq!(
                repo.lists_of(bob, survivor).await.unwrap(),
                [UserList::Favourites]
            );
        }

        #[tokio::test]
        async fn a_merge_is_between_two_titles_of_one_kind() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let movie = TitleRef::Movie(fixture.new_movie().await);
            let series = TitleRef::Show(fixture.new_show().await);
            repo.add(user, UserList::Watchlist, movie).await.unwrap();

            assert_eq!(
                repo.carry(movie, series).await.unwrap_err().to_string(),
                $crate::repositories::user_list::carry_kind_mismatch().to_string()
            );
            repo.carry(movie, movie).await.unwrap();
            assert_eq!(
                repo.lists_of(user, movie).await.unwrap(),
                [UserList::Watchlist],
                "neither a refused carry nor one onto itself moves anything"
            );

            let other = TitleRef::Show(fixture.new_show().await);
            repo.add(user, UserList::Favourites, series).await.unwrap();
            repo.carry(series, other).await.unwrap();
            assert_eq!(
                repo.lists_of(user, other).await.unwrap(),
                [UserList::Favourites]
            );
        }
    };
}
