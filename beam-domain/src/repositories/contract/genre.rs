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
            let slug = $crate::models::genre::slugify(&sci_fi);
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

        /// The genres a policy lets a caller see are those its titles carry:
        /// one only hidden titles carry is left out, and so is one no title
        /// carries any more (issue #328).
        #[tokio::test]
        async fn the_carried_genres_are_those_of_the_titles_the_policy_admits() {
            use ::std::collections::BTreeSet;

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
            let unique = Uuid::new_v4().simple().to_string();
            let genre = |name: &str| format!("{name} {unique}");
            repo.set_movie_genres(scenario.kids_movie, &[genre("Family")])
                .await
                .unwrap();
            repo.set_show_genres(scenario.kids_show, &[genre("Animation")])
                .await
                .unwrap();
            repo.set_movie_genres(scenario.grown_movie, &[genre("Crime"), genre("Family")])
                .await
                .unwrap();
            repo.set_show_genres(scenario.grown_show, &[genre("Orphaned")])
                .await
                .unwrap();
            repo.set_show_genres(scenario.grown_show, &[genre("Noir")])
                .await
                .unwrap();

            let suffix = unique.as_str();
            let carried = |access: $crate::models::access::AccessPolicy| async move {
                repo.find_carried(&access)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|g| g.name)
                    .filter(|name| name.ends_with(suffix))
                    .collect::<BTreeSet<String>>()
            };
            let named = |names: &[&str]| names.iter().map(|n| genre(n)).collect::<BTreeSet<_>>();
            assert_eq!(
                carried(scenario.kids_only()).await,
                named(&["Animation", "Family"])
            );
            assert_eq!(
                carried(scenario.both_under_parental_guidance()).await,
                named(&["Animation", "Family"])
            );
            assert_eq!(carried(scenario.grown_under_teen()).await, named(&[]));
            assert_eq!(
                carried($crate::models::access::AccessPolicy::unrestricted()).await,
                named(&["Animation", "Crime", "Family", "Noir"]),
                "a genre no title carries any more is not listed"
            );
        }
    };
}
