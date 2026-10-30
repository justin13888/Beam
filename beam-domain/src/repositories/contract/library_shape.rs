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
            Some(MediaFileContent::movie(movie_entry_id))
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
