/// Behavioural contract for [`crate::repositories::LibraryRepository`]
/// (issue #349): a library reads back as created, by id and in the list; a
/// scan's progress writes only the fields it is given, and for a library that
/// does not exist -- deleted while its scan ran -- it is a no-op, as the user
/// store's `set_admin` is for an unknown id;
/// a library's file count is of its own present files, read from the file
/// store; its title count is of the movies, shows, seasons and episodes its
/// own present files reach (issue #494); and a deleted library is gone, deleting an unknown one changing
/// nothing.
///
/// `$setup` names an `async fn() -> impl LibraryRepositoryFixture` whose
/// store starts empty.
#[macro_export]
macro_rules! library_repository_contract {
    ($setup:path) => {
        use ::chrono::{DateTime, TimeZone, Utc};
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::library::{CreateLibrary, Library};
        use $crate::repositories::contract::fixture::LibraryRepositoryFixture;
        use $crate::test_utils::{create_library, create_media_file};

        /// A whole second, so no store rounds it.
        fn at(secs: i64) -> DateTime<Utc> {
            Utc.timestamp_opt(1_900_000_000 + secs, 0).unwrap()
        }

        async fn library(fixture: &impl LibraryRepositoryFixture, name: &str) -> Library {
            fixture.repo().create(create_library(name)).await.unwrap()
        }

        /// A present file in `library_id`, returning its id.
        async fn file_in(fixture: &impl LibraryRepositoryFixture, library_id: Uuid) -> Uuid {
            fixture
                .files()
                .create(create_media_file(
                    library_id,
                    format!("/media/{library_id}/{}.mkv", Uuid::new_v4()),
                ))
                .await
                .unwrap()
                .id
        }

        fn scan(library: &Library) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<i32>) {
            (
                library.last_scan_started_at,
                library.last_scan_finished_at,
                library.last_scan_file_count,
            )
        }

        #[tokio::test]
        async fn a_library_reads_back_as_created_by_id_and_in_the_list() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            assert!(repo.find_all().await.unwrap().is_empty());
            assert_eq!(repo.count().await.unwrap(), 0);

            let created = repo
                .create(CreateLibrary {
                    name: "Films".to_string(),
                    root_path: PathBuf::from("/media/films"),
                    description: Some("Everything on the NAS".to_string()),
                })
                .await
                .unwrap();
            let other = library(&fixture, "Series").await;

            let found = repo.find_by_id(created.id).await.unwrap().unwrap();
            for library in [&created, &found] {
                let Library {
                    id,
                    name,
                    root_path,
                    description,
                    created_at: _,
                    updated_at: _,
                    last_scan_started_at,
                    last_scan_finished_at,
                    last_scan_file_count,
                } = library;
                assert_eq!(*id, created.id);
                assert_eq!(name, "Films");
                assert_eq!(root_path, &PathBuf::from("/media/films"));
                assert_eq!(description.as_deref(), Some("Everything on the NAS"));
                assert_eq!(
                    (
                        *last_scan_started_at,
                        *last_scan_finished_at,
                        *last_scan_file_count
                    ),
                    (None, None, None),
                    "a new library has never been scanned"
                );
            }
            assert_eq!(
                (found.created_at, found.updated_at),
                (created.created_at, created.updated_at)
            );
            assert_ne!(created.id, other.id);

            let mut all: Vec<Uuid> = repo
                .find_all()
                .await
                .unwrap()
                .iter()
                .map(|l| l.id)
                .collect();
            all.sort();
            let mut expected = vec![created.id, other.id];
            expected.sort();
            assert_eq!(all, expected);
            assert_eq!(repo.count().await.unwrap(), 2);
            assert!(repo.find_by_id(Uuid::new_v4()).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn scan_progress_writes_only_the_fields_it_is_given() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let films = library(&fixture, "films").await;
            let other = library(&fixture, "other").await;

            repo.update_scan_progress(films.id, Some(at(0)), None, None)
                .await
                .unwrap();
            let read = repo.find_by_id(films.id).await.unwrap().unwrap();
            assert_eq!(scan(&read), (Some(at(0)), None, None));

            repo.update_scan_progress(films.id, None, Some(at(30)), Some(12))
                .await
                .unwrap();
            let read = repo.find_by_id(films.id).await.unwrap().unwrap();
            assert_eq!(scan(&read), (Some(at(0)), Some(at(30)), Some(12)));

            // Nothing given writes nothing.
            repo.update_scan_progress(films.id, None, None, None)
                .await
                .unwrap();
            let read = repo.find_by_id(films.id).await.unwrap().unwrap();
            assert_eq!(scan(&read), (Some(at(0)), Some(at(30)), Some(12)));

            // The next scan starts over the last one's record.
            repo.update_scan_progress(films.id, Some(at(60)), None, None)
                .await
                .unwrap();
            let read = repo.find_by_id(films.id).await.unwrap().unwrap();
            assert_eq!(scan(&read), (Some(at(60)), Some(at(30)), Some(12)));

            let untouched = repo.find_by_id(other.id).await.unwrap().unwrap();
            assert_eq!(
                scan(&untouched),
                (None, None, None),
                "libraries are kept apart"
            );
        }

        #[tokio::test]
        async fn scan_progress_for_a_library_that_does_not_exist_is_a_no_op() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let films = library(&fixture, "films").await;
            let missing = Uuid::new_v4();

            repo.update_scan_progress(missing, Some(at(0)), Some(at(5)), Some(3))
                .await
                .unwrap();
            repo.update_scan_progress(missing, None, None, None)
                .await
                .unwrap();

            assert!(repo.find_by_id(missing).await.unwrap().is_none());
            assert_eq!(repo.count().await.unwrap(), 1, "no library was made");
            let read = repo.find_by_id(films.id).await.unwrap().unwrap();
            assert_eq!(scan(&read), (None, None, None));
        }

        #[tokio::test]
        async fn a_library_counts_only_its_own_present_files() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let films = library(&fixture, "films").await;
            let series = library(&fixture, "series").await;
            let empty = library(&fixture, "empty").await;
            for _ in 0..3 {
                file_in(&fixture, films.id).await;
            }
            let gone = file_in(&fixture, films.id).await;
            file_in(&fixture, series.id).await;
            fixture
                .files()
                .mark_missing(vec![gone], at(0))
                .await
                .unwrap();

            assert_eq!(
                repo.count_files(films.id).await.unwrap(),
                3,
                "a missing file is not counted"
            );
            assert_eq!(repo.count_files(series.id).await.unwrap(), 1);
            assert_eq!(repo.count_files(empty.id).await.unwrap(), 0);
            assert_eq!(repo.count_files(Uuid::new_v4()).await.unwrap(), 0);
        }

        /// A present file in `library_id` holding `content`, returning its id.
        async fn title_file_in(
            fixture: &impl LibraryRepositoryFixture,
            library_id: Uuid,
            content: $crate::models::file::MediaFileContent,
        ) -> Uuid {
            fixture
                .files()
                .create($crate::models::file::CreateMediaFile {
                    content: Some(content),
                    status: $crate::models::file::FileStatus::Known,
                    ..create_media_file(
                        library_id,
                        format!("/media/{library_id}/{}.mkv", Uuid::new_v4()),
                    )
                })
                .await
                .unwrap()
                .id
        }

        /// A movie entry, in `library_id`, of the movie `title` names.
        async fn movie_entry(
            fixture: &impl LibraryRepositoryFixture,
            library_id: Uuid,
            title: &str,
            edition: Option<&str>,
        ) -> Uuid {
            let movie = fixture
                .movies()
                .find_or_create_by_identity($crate::models::movie::CreateMovie::new(
                    title.to_string(),
                    None,
                    None,
                ))
                .await
                .unwrap();
            fixture
                .movies()
                .find_or_create_entry($crate::models::movie::CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: edition.map(str::to_string),
                })
                .await
                .unwrap()
                .id
        }

        /// The episode numbered `episode` of season `season` of the show
        /// `title` names.
        async fn episode_of(
            fixture: &impl LibraryRepositoryFixture,
            title: &str,
            season: u32,
            episode: u32,
        ) -> Uuid {
            let show = fixture
                .shows()
                .find_or_create_by_identity($crate::models::show::CreateShow::new(
                    title.to_string(),
                    None,
                ))
                .await
                .unwrap();
            let season = fixture
                .shows()
                .find_or_create_season(show.id, season)
                .await
                .unwrap();
            fixture
                .shows()
                .find_or_create_episode($crate::models::show::CreateEpisode {
                    season_id: season.id,
                    episode_number: episode,
                    title: format!("Episode {episode}"),
                    runtime: None,
                    air_date: None,
                    numbering: $crate::utils::media_path::EpisodeNumbering::Standard,
                })
                .await
                .unwrap()
                .id
        }

        #[tokio::test]
        async fn a_library_counts_only_its_own_live_titles_by_kind() {
            use $crate::models::file::MediaFileContent;
            use $crate::models::library::LibraryTitleCounts;

            let fixture = $setup().await;
            let repo = fixture.repo();
            let films = library(&fixture, "films").await;
            let series = library(&fixture, "series").await;
            let empty = library(&fixture, "empty").await;

            // Two editions of one movie are one title; a movie whose only
            // file is missing is not live; an unclassified file is no title.
            let theatrical = movie_entry(&fixture, films.id, "Heat", None).await;
            let directors = movie_entry(&fixture, films.id, "Heat", Some("Director's Cut")).await;
            title_file_in(&fixture, films.id, MediaFileContent::movie(theatrical)).await;
            title_file_in(&fixture, films.id, MediaFileContent::movie(directors)).await;
            let gone_entry = movie_entry(&fixture, films.id, "Ronin", None).await;
            let gone = title_file_in(&fixture, films.id, MediaFileContent::movie(gone_entry)).await;
            file_in(&fixture, films.id).await;

            // Two seasons of one show, an episode with two present files, and
            // an episode whose only file is missing.
            let pilot = episode_of(&fixture, "Lost", 1, 1).await;
            for _ in 0..2 {
                title_file_in(&fixture, series.id, MediaFileContent::episode(pilot)).await;
            }
            let second = episode_of(&fixture, "Lost", 1, 2).await;
            title_file_in(&fixture, series.id, MediaFileContent::episode(second)).await;
            let premiere = episode_of(&fixture, "Lost", 2, 1).await;
            title_file_in(&fixture, series.id, MediaFileContent::episode(premiere)).await;
            let missing_episode = episode_of(&fixture, "Lost", 3, 1).await;
            let gone_episode = title_file_in(
                &fixture,
                series.id,
                MediaFileContent::episode(missing_episode),
            )
            .await;
            fixture
                .files()
                .mark_missing(vec![gone, gone_episode], at(0))
                .await
                .unwrap();

            // The same show's episode filed in the films library counts there
            // too, and adds nothing to the series library.
            title_file_in(&fixture, films.id, MediaFileContent::episode(pilot)).await;

            assert_eq!(
                repo.count_titles(films.id).await.unwrap(),
                LibraryTitleCounts {
                    movies: 1,
                    shows: 1,
                    seasons: 1,
                    episodes: 1,
                },
                "a missing file keeps no title live"
            );
            assert_eq!(
                repo.count_titles(series.id).await.unwrap(),
                LibraryTitleCounts {
                    movies: 0,
                    shows: 1,
                    seasons: 2,
                    episodes: 3,
                }
            );
            assert_eq!(
                repo.count_titles(empty.id).await.unwrap(),
                LibraryTitleCounts::default()
            );
            assert_eq!(
                repo.count_titles(Uuid::new_v4()).await.unwrap(),
                LibraryTitleCounts::default()
            );
        }

        #[tokio::test]
        async fn a_deleted_library_is_gone_and_deleting_an_unknown_one_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let films = library(&fixture, "films").await;
            let series = library(&fixture, "series").await;

            repo.delete(films.id).await.unwrap();
            assert!(repo.find_by_id(films.id).await.unwrap().is_none());
            assert_eq!(repo.count().await.unwrap(), 1);

            repo.delete(films.id).await.unwrap();
            repo.delete(Uuid::new_v4()).await.unwrap();
            let all: Vec<Uuid> = repo
                .find_all()
                .await
                .unwrap()
                .iter()
                .map(|l| l.id)
                .collect();
            assert_eq!(all, vec![series.id]);
        }
    };
}
