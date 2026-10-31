/// Behavioural contract for [`crate::repositories::CollectionRepository`]: a
/// collection is one row per provider id however many movies are in it, a
/// movie is in at most one, and the listing holds only collections some
/// movie is in, by name, a page at a time (issue #262).
///
/// `$setup` names an `async fn() -> impl CollectionRepositoryFixture`.
#[macro_export]
macro_rules! collection_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::movie::CreateMovie;
        use $crate::providers::enrichment::{CollectionEnrichment, ExternalMediaRef};
        use $crate::repositories::contract::fixture::CollectionRepositoryFixture;

        async fn new_movie(fixture: &impl CollectionRepositoryFixture) -> Uuid {
            fixture
                .movies()
                .find_or_create_by_identity(CreateMovie::new(
                    format!("collected movie {}", Uuid::new_v4()),
                    None,
                    None,
                ))
                .await
                .unwrap()
                .id
        }

        fn collection(native: &str, name: &str) -> CollectionEnrichment {
            CollectionEnrichment {
                collection_ref: ExternalMediaRef::new("tmdb", native),
                name: name.to_string(),
                poster_url: Some(format!("https://image.example/{native}-poster.jpg")),
                backdrop_url: None,
            }
        }

        fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
            ids.sort();
            ids
        }

        /// A policy nobody restricted.
        fn open() -> $crate::models::access::AccessPolicy {
            $crate::models::access::AccessPolicy::unrestricted()
        }

        #[tokio::test]
        async fn movies_reporting_one_collection_share_one_row() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (first, second) = (new_movie(&fixture).await, new_movie(&fixture).await);
            repo.set_movie_collection(first, Some(&collection("2344", "The Matrix Collection")))
                .await
                .unwrap();
            repo.set_movie_collection(second, Some(&collection("2344", "The Matrix Collection")))
                .await
                .unwrap();

            let of_first = repo
                .movie_collection(first)
                .await
                .unwrap()
                .expect("the first movie's collection");
            let of_second = repo
                .movie_collection(second)
                .await
                .unwrap()
                .expect("the second movie's collection");
            assert_eq!(of_first, of_second);
            assert_eq!(of_first.external_ref, "tmdb:2344");
            assert_eq!(of_first.name, "The Matrix Collection");
            assert_eq!(
                of_first.poster_url.as_deref(),
                Some("https://image.example/2344-poster.jpg")
            );
            assert_eq!(
                repo.find_by_id(of_first.id).await.unwrap(),
                Some(of_first.clone())
            );
            assert_eq!(
                sorted(repo.movie_ids(of_first.id, &open()).await.unwrap()),
                sorted(vec![first, second])
            );
            assert!(repo.find_by_id(Uuid::new_v4()).await.unwrap().is_none());
            assert!(
                repo.movie_ids(Uuid::new_v4(), &open())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn a_later_report_refreshes_the_collection_and_moves_the_movie() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (movie, other) = (new_movie(&fixture).await, new_movie(&fixture).await);
            repo.set_movie_collection(movie, Some(&collection("1", "Old Name")))
                .await
                .unwrap();
            repo.set_movie_collection(other, Some(&collection("1", "Old Name")))
                .await
                .unwrap();
            let original = repo.movie_collection(movie).await.unwrap().unwrap();

            let renamed = CollectionEnrichment {
                backdrop_url: Some("https://image.example/1-backdrop.jpg".to_string()),
                ..collection("1", "New Name")
            };
            repo.set_movie_collection(other, Some(&renamed))
                .await
                .unwrap();
            let refreshed = repo.movie_collection(movie).await.unwrap().unwrap();
            assert_eq!(refreshed.id, original.id, "one provider id, one row");
            assert_eq!(refreshed.name, "New Name");
            assert_eq!(
                refreshed.backdrop_url.as_deref(),
                Some("https://image.example/1-backdrop.jpg")
            );

            repo.set_movie_collection(movie, Some(&collection("2", "Another")))
                .await
                .unwrap();
            assert_eq!(
                repo.movie_collection(movie).await.unwrap().unwrap().name,
                "Another",
                "a movie is in one collection at a time"
            );
            assert_eq!(repo.movie_ids(original.id, &open()).await.unwrap(), [other]);

            repo.set_movie_collection(movie, None).await.unwrap();
            assert!(repo.movie_collection(movie).await.unwrap().is_none());
            repo.set_movie_collection(movie, None).await.unwrap();
        }

        #[tokio::test]
        async fn the_listing_holds_collections_with_movies_by_name_a_page_at_a_time() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            for (native, name) in [
                ("10", "zeta Saga"),
                ("11", "Alien Collection"),
                ("12", "Mad Max Collection"),
                // Folding only ASCII case puts `É` (U+00C9) after `z`.
                ("13", "Évasion"),
            ] {
                let movie = new_movie(&fixture).await;
                repo.set_movie_collection(movie, Some(&collection(native, name)))
                    .await
                    .unwrap();
            }
            let second_alien = new_movie(&fixture).await;
            repo.set_movie_collection(second_alien, Some(&collection("11", "Alien Collection")))
                .await
                .unwrap();
            // A collection its only movie left is not listed.
            let leaver = new_movie(&fixture).await;
            repo.set_movie_collection(leaver, Some(&collection("14", "Abandoned")))
                .await
                .unwrap();
            repo.set_movie_collection(leaver, None).await.unwrap();

            let names = |page: Vec<$crate::models::collection::CollectionSummary>| {
                page.into_iter()
                    .map(|s| (s.collection.name, s.movie_count))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                names(repo.list(&open(), 2, 0).await.unwrap()),
                [
                    ("Alien Collection".to_string(), 2),
                    ("Mad Max Collection".to_string(), 1)
                ]
            );
            assert_eq!(
                names(repo.list(&open(), 2, 2).await.unwrap()),
                [("zeta Saga".to_string(), 1), ("Évasion".to_string(), 1)]
            );
            assert!(repo.list(&open(), 2, 4).await.unwrap().is_empty());
            assert_eq!(repo.count(&open()).await.unwrap(), 4);
        }

        /// The listing, its count and a collection's movies hold only the
        /// movies a policy admits: a collection whose every movie it hides
        /// is not listed, and one partly hidden counts what is left
        /// (issue #328).
        #[tokio::test]
        async fn the_listing_and_a_collections_movies_hold_only_what_the_policy_admits() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (kids, grown) = (fixture.new_library().await, fixture.new_library().await);
            let scenario = $crate::repositories::contract::fixture::AccessScenario::build(
                fixture.movies(),
                fixture.shows(),
                kids,
                grown,
            )
            .await;
            let grown_sequel = $crate::repositories::contract::fixture::AccessScenario::build(
                fixture.movies(),
                fixture.shows(),
                kids,
                grown,
            )
            .await
            .grown_movie;
            let mixed = collection("20", "A Mixed Saga");
            let grown_only = collection("21", "B Grown Saga");
            repo.set_movie_collection(scenario.kids_movie, Some(&mixed))
                .await
                .unwrap();
            repo.set_movie_collection(scenario.grown_movie, Some(&mixed))
                .await
                .unwrap();
            repo.set_movie_collection(grown_sequel, Some(&grown_only))
                .await
                .unwrap();
            let mixed_id = repo
                .movie_collection(scenario.kids_movie)
                .await
                .unwrap()
                .unwrap()
                .id;
            let grown_id = repo
                .movie_collection(grown_sequel)
                .await
                .unwrap()
                .unwrap()
                .id;

            let names = |page: Vec<$crate::models::collection::CollectionSummary>| {
                page.into_iter()
                    .map(|s| (s.collection.name, s.movie_count))
                    .collect::<Vec<_>>()
            };
            for policy in [
                scenario.kids_only(),
                scenario.both_under_parental_guidance(),
            ] {
                assert_eq!(
                    names(repo.list(&policy, 10, 0).await.unwrap()),
                    [("A Mixed Saga".to_string(), 1)],
                    "{policy:?}"
                );
                assert_eq!(repo.count(&policy).await.unwrap(), 1, "{policy:?}");
                assert_eq!(
                    repo.movie_ids(mixed_id, &policy).await.unwrap(),
                    [scenario.kids_movie],
                    "{policy:?}"
                );
                assert!(
                    repo.movie_ids(grown_id, &policy).await.unwrap().is_empty(),
                    "{policy:?}"
                );
            }
            let nothing = scenario.grown_under_teen();
            assert!(repo.list(&nothing, 10, 0).await.unwrap().is_empty());
            assert_eq!(repo.count(&nothing).await.unwrap(), 0);
            assert_eq!(
                sorted(repo.movie_ids(mixed_id, &open()).await.unwrap()),
                sorted(vec![scenario.kids_movie, scenario.grown_movie])
            );
        }
    };
}
