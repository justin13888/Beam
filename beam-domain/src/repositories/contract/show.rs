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

        /// A season's enrichment fills the season and the episodes scanning
        /// already made, an episode field it does not carry left as it was,
        /// and makes no season or episode that no file stands for.
        #[tokio::test]
        async fn a_seasons_enrichment_fills_its_existing_episodes_and_fabricates_none() {
            use ::chrono::NaiveDate;
            use $crate::providers::enrichment::{EpisodeEnrichment, SeasonEnrichment};

            let fixture = $setup().await;
            let repo = fixture.repo();
            let show = repo
                .find_or_create_by_identity(new_show("season enrichment"))
                .await
                .unwrap();
            let one = repo.find_or_create_season(show.id, 1).await.unwrap();
            let two = repo.find_or_create_season(show.id, 2).await.unwrap();
            let pilot = repo
                .find_or_create_episode(episode(one.id, 1, "Pilot", 30))
                .await
                .unwrap();
            let second = repo
                .find_or_create_episode(episode(one.id, 2, "Two", 30))
                .await
                .unwrap();
            let elsewhere = repo
                .find_or_create_episode(episode(two.id, 1, "S2E1", 30))
                .await
                .unwrap();
            let aired = NaiveDate::from_ymd_opt(2005, 3, 24).unwrap();

            let updated = repo
                .apply_season_enrichment(
                    show.id,
                    &SeasonEnrichment {
                        season_number: 1,
                        poster_url: Some("https://img.example/s1.jpg".to_string()),
                        air_date: Some(aired),
                        episodes: vec![
                            EpisodeEnrichment {
                                episode_number: 1,
                                absolute_number: Some(1),
                                title: Some("Pilot (Diversity Day)".to_string()),
                                description: Some(
                                    "The office meets a documentary crew.".to_string(),
                                ),
                                air_date: Some(aired),
                                runtime_mins: Some(22),
                                thumbnail_url: Some("https://img.example/e1.jpg".to_string()),
                            },
                            // Carries nothing, so changes nothing -- but it
                            // still names an episode that exists.
                            EpisodeEnrichment {
                                episode_number: 2,
                                ..EpisodeEnrichment::default()
                            },
                            // No file stands for it: skipped, not made.
                            EpisodeEnrichment {
                                episode_number: 9,
                                title: Some("Fabricated".to_string()),
                                ..EpisodeEnrichment::default()
                            },
                        ],
                    },
                )
                .await
                .unwrap();
            assert_eq!(updated, 2, "the two episodes that exist");

            let season = repo.find_season_by_id(one.id).await.unwrap().unwrap();
            assert_eq!(
                season.poster_url.as_deref(),
                Some("https://img.example/s1.jpg")
            );
            assert_eq!(season.first_aired, Some(aired));

            let read = repo.find_episode_by_id(pilot.id).await.unwrap().unwrap();
            assert_eq!(read.title, "Pilot (Diversity Day)");
            assert_eq!(
                read.description.as_deref(),
                Some("The office meets a documentary crew.")
            );
            assert_eq!(read.air_date, Some(aired));
            assert_eq!(read.runtime, Some(Duration::from_secs(22 * 60)));
            assert_eq!(
                read.thumbnail_url.as_deref(),
                Some("https://img.example/e1.jpg")
            );

            let read = repo.find_episode_by_id(second.id).await.unwrap().unwrap();
            assert_eq!(
                (
                    read.title.as_str(),
                    read.description,
                    read.air_date,
                    read.runtime,
                    read.thumbnail_url
                ),
                ("Two", None, None, Some(Duration::from_secs(30 * 60)), None),
                "a field the enrichment does not carry is left as it was"
            );

            let mut numbers: Vec<u32> = repo
                .find_episodes_by_season_id(one.id)
                .await
                .unwrap()
                .iter()
                .map(|e| e.episode_number)
                .collect();
            numbers.sort_unstable();
            assert_eq!(numbers, [1, 2], "no episode 9 was made");
            let untouched = repo
                .find_episode_by_id(elsewhere.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(untouched.title, "S2E1");
            let other_season = repo.find_season_by_id(two.id).await.unwrap().unwrap();
            assert_eq!(
                (other_season.poster_url, other_season.first_aired),
                (None, None)
            );

            // A season no file stands for is neither filled nor made, and nor
            // is anything of a show that does not exist.
            let missing_season = SeasonEnrichment {
                season_number: 7,
                poster_url: Some("https://img.example/s7.jpg".to_string()),
                ..SeasonEnrichment::default()
            };
            assert_eq!(
                repo.apply_season_enrichment(show.id, &missing_season)
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(
                repo.find_seasons_by_show_id(show.id).await.unwrap().len(),
                2
            );
            assert_eq!(
                repo.apply_season_enrichment(Uuid::new_v4(), &missing_season)
                    .await
                    .unwrap(),
                0
            );
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
            assert_eq!(stored.air_date, Some(aired));
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
                    tvdb_id: Some(121361),
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
                ProviderPin::Tvdb(121361),
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
                    tvdb_id: Some(81189),
                    title: "Breaking Bad".to_string(),
                    original_title: Some("Breaking Bad (orig)".to_string()),
                    description: Some("A teacher turns.".to_string()),
                    year: Some(2008),
                    poster_url: Some("https://img/p.jpg".to_string()),
                    backdrop_url: Some("https://img/b.jpg".to_string()),
                    rating: Some(9.5),
                    content_rating: Some("TV-MA".to_string()),
                    genres: Vec::new(),
                    credits: None,
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
            assert_eq!(after.tvdb_id, Some(81189), "as is its TheTVDB id");
            assert_eq!(
                after.content_rating.as_deref(),
                Some("TV-MA"),
                "the certification is no lockable field, so it is always written"
            );

            repo.apply_enrichment(
                title.id,
                &ShowEnrichment {
                    title: after.title.clone(),
                    ..Default::default()
                },
                &FieldLocks::none(),
            )
            .await
            .unwrap();
            assert_eq!(
                repo.find_by_id(title.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .content_rating,
                None,
                "a provider reporting none clears it"
            );
        }

        #[tokio::test]
        async fn library_ids_are_the_libraries_each_show_is_associated_with() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (one, two) = (fixture.new_library().await, fixture.new_library().await);
            let both = repo
                .find_or_create_by_identity(new_show("In both"))
                .await
                .unwrap();
            let unassociated = repo
                .find_or_create_by_identity(new_show("In none"))
                .await
                .unwrap();
            for library in [one, two, two] {
                repo.ensure_library_association(library, both.id)
                    .await
                    .unwrap();
            }

            let libraries = repo
                .library_ids(&[both.id, unassociated.id, Uuid::new_v4()])
                .await
                .unwrap();
            assert_eq!(libraries.len(), 1, "{libraries:?}");
            assert_eq!(libraries[&both.id], [one, two].into_iter().collect());
            assert!(repo.library_ids(&[]).await.unwrap().is_empty());
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
