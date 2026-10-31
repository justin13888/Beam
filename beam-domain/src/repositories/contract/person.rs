/// Behavioural contract for [`crate::repositories::PersonRepository`]: a
/// person is one row per provider id however many titles credit them, a
/// title's credits are replaced whole, a filmography spans movies and shows,
/// and search lists credited people by name, a page at a time.
///
/// `$setup` names an `async fn() -> impl PersonRepositoryFixture`.
#[macro_export]
macro_rules! person_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::catalog::TitleKind;
        use $crate::models::movie::CreateMovie;
        use $crate::models::person::CreditRole;
        use $crate::models::show::CreateShow;
        use $crate::providers::enrichment::{CreditEnrichment, ExternalMediaRef};
        use $crate::repositories::contract::fixture::PersonRepositoryFixture;

        async fn new_movie(fixture: &impl PersonRepositoryFixture) -> Uuid {
            fixture
                .movies()
                .find_or_create_by_identity(CreateMovie::new(
                    format!("credited movie {}", Uuid::new_v4()),
                    None,
                    None,
                ))
                .await
                .unwrap()
                .id
        }

        async fn new_show(fixture: &impl PersonRepositoryFixture) -> Uuid {
            fixture
                .shows()
                .find_or_create_by_identity(CreateShow::new(
                    format!("credited show {}", Uuid::new_v4()),
                    None,
                ))
                .await
                .unwrap()
                .id
        }

        fn cast(native: &str, name: &str, character: &str, billing_order: u32) -> CreditEnrichment {
            CreditEnrichment {
                person_ref: ExternalMediaRef::new("tmdb", native),
                name: name.to_string(),
                profile_url: Some(format!("https://image.example/{native}.jpg")),
                role: CreditRole::Cast {
                    character: Some(character.to_string()),
                    billing_order,
                },
            }
        }

        fn crew(native: &str, name: &str, job: &str) -> CreditEnrichment {
            CreditEnrichment {
                person_ref: ExternalMediaRef::new("tmdb", native),
                name: name.to_string(),
                profile_url: None,
                role: CreditRole::Crew {
                    job: job.to_string(),
                    department: Some("Directing".to_string()),
                },
            }
        }

        /// A policy nobody restricted.
        fn open() -> $crate::models::access::AccessPolicy {
            $crate::models::access::AccessPolicy::unrestricted()
        }

        /// `(provider id, role)` of each credit, sorted, for comparing sets.
        fn roles(credits: &[$crate::models::person::TitleCredit]) -> Vec<(String, String)> {
            let mut roles: Vec<(String, String)> = credits
                .iter()
                .map(|c| (c.person.external_ref.clone(), format!("{:?}", c.role)))
                .collect();
            roles.sort();
            roles
        }

        #[tokio::test]
        async fn a_titles_credits_read_back_with_their_people() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let series = new_show(&fixture).await;
            repo.set_movie_credits(
                movie,
                &[
                    cast("1", "Keanu Reeves", "Neo", 0),
                    crew("2", "Lana Wachowski", "Director"),
                ],
            )
            .await
            .unwrap();
            repo.set_show_credits(series, &[cast("3", "Bryan Cranston", "Walter White", 0)])
                .await
                .unwrap();

            let credits = repo.movie_credits(movie).await.unwrap();
            assert_eq!(credits.len(), 2);
            let neo = credits
                .iter()
                .find(|c| c.person.external_ref == "tmdb:1")
                .expect("the cast credit");
            assert_eq!(neo.person.name, "Keanu Reeves");
            assert_eq!(
                neo.person.profile_url.as_deref(),
                Some("https://image.example/1.jpg")
            );
            assert_eq!(
                neo.role,
                CreditRole::Cast {
                    character: Some("Neo".to_string()),
                    billing_order: 0,
                }
            );
            let director = credits
                .iter()
                .find(|c| c.person.external_ref == "tmdb:2")
                .expect("the crew credit");
            assert_eq!(
                director.role,
                CreditRole::Crew {
                    job: "Director".to_string(),
                    department: Some("Directing".to_string()),
                }
            );
            assert_eq!(director.person.profile_url, None);

            let show_credits = repo.show_credits(series).await.unwrap();
            assert_eq!(roles(&show_credits).len(), 1);
            assert_eq!(show_credits[0].person.name, "Bryan Cranston");
            assert!(
                repo.show_credits(movie).await.unwrap().is_empty(),
                "a movie's credits are not read as a show's"
            );
            assert!(repo.movie_credits(series).await.unwrap().is_empty());

            let person = repo
                .find_by_id(neo.person.id)
                .await
                .unwrap()
                .expect("the person is stored");
            assert_eq!(person, neo.person);
            assert_eq!(repo.find_by_id(Uuid::new_v4()).await.unwrap(), None);
        }

        #[tokio::test]
        async fn setting_credits_replaces_the_set_and_keeps_one_person_per_provider_id() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let first = new_movie(&fixture).await;
            let second = new_movie(&fixture).await;

            repo.set_movie_credits(
                first,
                &[
                    cast("10", "Carrie-Anne Moss", "Trinity", 1),
                    crew("11", "Old Crew", "Director"),
                ],
            )
            .await
            .unwrap();
            // The second set replaces the first, and a credit listed twice is
            // stored once.
            repo.set_movie_credits(
                first,
                &[
                    cast("10", "Carrie-Anne Moss", "Trinity", 1),
                    cast("10", "Carrie-Anne Moss", "Trinity", 1),
                ],
            )
            .await
            .unwrap();
            // The same person on another title, under a refreshed name and
            // portrait, is the person already stored.
            let mut renamed = cast("10", "Carrie Anne Moss", "Trinity", 0);
            renamed.profile_url = Some("https://image.example/new.jpg".to_string());
            repo.set_movie_credits(second, &[renamed]).await.unwrap();

            let first_credits = repo.movie_credits(first).await.unwrap();
            assert_eq!(first_credits.len(), 1, "{first_credits:?}");
            let second_credits = repo.movie_credits(second).await.unwrap();
            assert_eq!(second_credits.len(), 1);
            assert_eq!(first_credits[0].person.id, second_credits[0].person.id);
            assert_eq!(second_credits[0].person.name, "Carrie Anne Moss");
            assert_eq!(
                first_credits[0].person.name, "Carrie Anne Moss",
                "a person has one name, the latest a provider gave"
            );
            assert_eq!(
                first_credits[0].person.profile_url.as_deref(),
                Some("https://image.example/new.jpg")
            );

            repo.set_movie_credits(first, &[]).await.unwrap();
            assert!(repo.movie_credits(first).await.unwrap().is_empty());
            assert_eq!(repo.movie_credits(second).await.unwrap().len(), 1);
        }

        #[tokio::test]
        async fn a_filmography_spans_movies_and_shows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let series = new_show(&fixture).await;
            repo.set_movie_credits(
                movie,
                &[
                    cast("20", "Actor", "Hero", 0),
                    crew("20", "Actor", "Director"),
                ],
            )
            .await
            .unwrap();
            repo.set_show_credits(series, &[cast("20", "Actor", "Narrator", 4)])
                .await
                .unwrap();

            let person = repo.movie_credits(movie).await.unwrap()[0].person.id;
            let mut filmography: Vec<(TitleKind, Uuid, String)> = repo
                .filmography(person, &open())
                .await
                .unwrap()
                .into_iter()
                .map(|c| (c.title_kind, c.title_id, format!("{:?}", c.role)))
                .collect();
            filmography.sort_by(|a, b| a.2.cmp(&b.2));
            assert_eq!(filmography.len(), 3, "{filmography:?}");
            assert!(
                filmography
                    .iter()
                    .any(|(kind, id, role)| *kind == TitleKind::Movie
                        && *id == movie
                        && role.contains("Hero"))
            );
            assert!(
                filmography
                    .iter()
                    .any(|(kind, id, role)| *kind == TitleKind::Movie
                        && *id == movie
                        && role.contains("Director"))
            );
            assert!(
                filmography
                    .iter()
                    .any(|(kind, id, role)| *kind == TitleKind::Show
                        && *id == series
                        && role.contains("Narrator"))
            );
            assert!(
                repo.filmography(Uuid::new_v4(), &open())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn search_lists_credited_people_by_name_a_page_at_a_time() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = new_movie(&fixture).await;
            let other = new_movie(&fixture).await;
            repo.set_movie_credits(
                movie,
                &[
                    cast("30", "zoe Smith", "A", 0),
                    cast("31", "Adam Smithers", "B", 1),
                    cast("32", "Mia Jones", "C", 2),
                    cast("33", "100%_Real", "D", 3),
                    // Non-ASCII names: folding all of Unicode would put
                    // "édith" before "Éva"; folding only ASCII leaves `É`
                    // (U+00C9) before `é` (U+00E9), and both after `z`.
                    cast("35", "édith Piaf", "F", 4),
                    cast("36", "Éva Green", "G", 5),
                ],
            )
            .await
            .unwrap();
            // Credited once, then no more: no longer listed.
            repo.set_movie_credits(other, &[cast("34", "Gone Smith", "E", 0)])
                .await
                .unwrap();
            repo.set_movie_credits(other, &[]).await.unwrap();

            let names = |people: Vec<$crate::models::person::Person>| -> Vec<String> {
                people.into_iter().map(|p| p.name).collect()
            };
            assert_eq!(
                names(repo.search(Some("SMITH"), &open(), 10, 0).await.unwrap()),
                ["Adam Smithers", "zoe Smith"],
                "a case-insensitive substring, ordered by name ignoring case"
            );
            assert_eq!(repo.count(Some("smith"), &open()).await.unwrap(), 2);
            assert_eq!(
                names(repo.search(None, &open(), 2, 0).await.unwrap()),
                ["100%_Real", "Adam Smithers"]
            );
            assert_eq!(
                names(repo.search(None, &open(), 2, 2).await.unwrap()),
                ["Mia Jones", "zoe Smith"]
            );
            assert_eq!(
                names(repo.search(None, &open(), 2, 4).await.unwrap()),
                ["Éva Green", "édith Piaf"],
                "ordered by code point once only ASCII case is folded"
            );
            assert!(repo.search(None, &open(), 2, 6).await.unwrap().is_empty());
            assert_eq!(repo.count(None, &open()).await.unwrap(), 6);
            assert_eq!(
                names(repo.search(Some("%_"), &open(), 10, 0).await.unwrap()),
                ["100%_Real"],
                "`%` and `_` match only themselves"
            );
            assert_eq!(repo.count(Some("_"), &open()).await.unwrap(), 1);
            assert!(
                repo.search(Some("nobody"), &open(), 10, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        /// Search, its count and a filmography hold only what a policy
        /// admits: a person credited only on hidden titles is not found, and
        /// one credited on both keeps only their visible credits
        /// (issue #328).
        #[tokio::test]
        async fn search_and_a_filmography_hold_only_the_titles_the_policy_admits() {
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
            repo.set_movie_credits(
                scenario.kids_movie,
                &[
                    cast("40", "Kid Actor", "A", 0),
                    crew("41", "Shared Director", "Director"),
                ],
            )
            .await
            .unwrap();
            repo.set_movie_credits(
                scenario.grown_movie,
                &[
                    cast("42", "Grown Actor", "B", 0),
                    crew("41", "Shared Director", "Director"),
                ],
            )
            .await
            .unwrap();
            repo.set_show_credits(scenario.kids_show, &[cast("43", "Show Kid", "C", 0)])
                .await
                .unwrap();
            repo.set_show_credits(scenario.grown_show, &[cast("44", "Show Grown", "D", 0)])
                .await
                .unwrap();
            let person_of = |credits: Vec<$crate::models::person::TitleCredit>, name: &str| {
                credits
                    .into_iter()
                    .find(|c| c.person.name == name)
                    .expect("the credited person")
                    .person
                    .id
            };
            let director = person_of(
                repo.movie_credits(scenario.kids_movie).await.unwrap(),
                "Shared Director",
            );
            let grown_actor = person_of(
                repo.movie_credits(scenario.grown_movie).await.unwrap(),
                "Grown Actor",
            );

            let names = |people: Vec<$crate::models::person::Person>| {
                people.into_iter().map(|p| p.name).collect::<Vec<_>>()
            };
            for policy in [
                scenario.kids_only(),
                scenario.both_under_parental_guidance(),
            ] {
                assert_eq!(
                    names(repo.search(None, &policy, 2, 0).await.unwrap()),
                    ["Kid Actor", "Shared Director"],
                    "{policy:?}"
                );
                assert_eq!(
                    names(repo.search(None, &policy, 2, 2).await.unwrap()),
                    ["Show Kid"],
                    "{policy:?}"
                );
                assert_eq!(repo.count(None, &policy).await.unwrap(), 3, "{policy:?}");
                assert!(
                    repo.search(Some("grown"), &policy, 10, 0)
                        .await
                        .unwrap()
                        .is_empty(),
                    "{policy:?}"
                );
                assert_eq!(repo.count(Some("grown"), &policy).await.unwrap(), 0);
                let credits: Vec<(TitleKind, Uuid)> = repo
                    .filmography(director, &policy)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|c| (c.title_kind, c.title_id))
                    .collect();
                assert_eq!(
                    credits,
                    [(TitleKind::Movie, scenario.kids_movie)],
                    "{policy:?}"
                );
                assert!(
                    repo.filmography(grown_actor, &policy)
                        .await
                        .unwrap()
                        .is_empty(),
                    "{policy:?}"
                );
            }
            let nothing = scenario.grown_under_teen();
            assert!(repo.search(None, &nothing, 10, 0).await.unwrap().is_empty());
            assert_eq!(repo.count(None, &nothing).await.unwrap(), 0);
            assert_eq!(repo.count(None, &open()).await.unwrap(), 5);
            assert_eq!(repo.filmography(director, &open()).await.unwrap().len(), 2);
        }
    };
}
