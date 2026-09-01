/// Tests for DbMetadataService using in-memory repository fakes.
///
/// These tests exercise the full metadata service vertical slice without any
/// external infrastructure. All repositories are stateful in-memory fakes.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use crate::services::metadata::{
        DbMetadataService, MediaFilter, MediaSearchFilters, MediaSortField, MetadataService,
        SortOrder,
    };
    use beam_domain::models::movie::Movie;
    use beam_domain::models::{Episode, MediaFile, MediaFileContent, MovieEntry, Season, Show};
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

    // ---------------------------------------------------------------------------
    // Helper builders
    // ---------------------------------------------------------------------------

    fn make_movie(title: &str, year: Option<u32>) -> Movie {
        Movie {
            id: Uuid::new_v4(),
            title: title.to_string(),
            title_localized: None,
            description: None,
            year,
            release_date: None,
            runtime: Some(Duration::from_secs(7200)),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            rating_imdb: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_media_file(library_id: Uuid, content: MediaFileContent) -> MediaFile {
        use std::path::PathBuf;
        MediaFile {
            id: Uuid::new_v4(),
            library_id,
            path: PathBuf::from("/media/test.mp4"),
            hash: 0,
            size_bytes: 1024,
            mtime: None,
            mime_type: Some("video/mp4".to_string()),
            duration: Some(Duration::from_secs(7200)),
            container_format: Some("mp4".to_string()),
            content: Some(content),
            status: beam_domain::models::FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_service() -> DbMetadataService {
        DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        )
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_media_metadata_unknown_id_returns_none() {
        let service = make_service();
        let result = service
            .get_media_metadata(&Uuid::new_v4().to_string())
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_media_metadata_invalid_id_returns_none() {
        let service = make_service();
        let result = service.get_media_metadata("not-a-uuid").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_movie_metadata_returns_movie() {
        use crate::models::MediaMetadata;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());

        // Seed a movie
        let movie = make_movie("Test Movie", Some(2023));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);

        // Seed movie entry and file
        let library_id = Uuid::new_v4();
        let entry = MovieEntry {
            id: Uuid::new_v4(),
            library_id,
            movie_id,
            edition: None,
            is_primary: true,
            created_at: chrono::Utc::now(),
        };
        let entry_id = entry.id;
        movie_repo.entries.lock().unwrap().insert(entry.id, entry);

        let file = make_media_file(
            library_id,
            MediaFileContent::Movie {
                movie_entry_id: entry_id,
            },
        );
        file_repo.files.lock().unwrap().insert(file.id, file);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let result = service.get_media_metadata(&movie_id.to_string()).await;
        assert!(result.is_some());
        match result.unwrap() {
            MediaMetadata::Movie(m) => {
                assert_eq!(m.id, movie_id.to_string());
                assert_eq!(m.title.original, "Test Movie");
                assert_eq!(m.year, Some(2023));
                assert!(m.duration.is_some());
                assert!(m.file_id.is_some(), "file_id must point at the seeded file");
            }
            _ => panic!("Expected Movie metadata"),
        }
    }

    #[tokio::test]
    async fn test_get_show_metadata_returns_show() {
        use crate::models::MediaMetadata;

        let show_repo = Arc::new(InMemoryShowRepository::default());

        // Seed show
        let show = Show {
            id: Uuid::new_v4(),
            title: "Test Show".to_string(),
            title_localized: None,
            description: Some("A test show".to_string()),
            year: Some(2022),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let show_id = show.id;
        show_repo.shows.lock().unwrap().insert(show.id, show);

        // Seed season + episode
        let season = Season {
            id: Uuid::new_v4(),
            show_id,
            season_number: 1,
            poster_url: None,
            first_aired: None,
            last_aired: None,
        };
        let season_id = season.id;
        show_repo.seasons.lock().unwrap().insert(season.id, season);

        let ep = Episode {
            id: Uuid::new_v4(),
            season_id,
            episode_number: 1,
            title: "Pilot".to_string(),
            description: None,
            air_date: None,
            runtime: None,
            thumbnail_url: None,
            created_at: chrono::Utc::now(),
        };
        let episode_id = ep.id;
        show_repo.episodes.lock().unwrap().insert(ep.id, ep);

        let service = DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let result = service.get_media_metadata(&show_id.to_string()).await;
        assert!(result.is_some());
        match result.unwrap() {
            MediaMetadata::Show(s) => {
                assert_eq!(s.id, show_id.to_string());
                assert_eq!(s.title.original, "Test Show");
                assert_eq!(s.year, Some(2022));
                assert_eq!(s.seasons.len(), 1);
                assert_eq!(s.seasons[0].episodes.len(), 1);
                let episode = &s.seasons[0].episodes[0];
                assert_eq!(episode.id, episode_id.to_string());
                assert_eq!(episode.title, "Pilot");
                assert!(
                    episode.file_id.is_none(),
                    "no file seeded \u{2192} file_id should be None"
                );
            }
            _ => panic!("Expected Show metadata"),
        }
    }

    #[tokio::test]
    async fn test_search_media_no_filter_returns_movies_and_shows() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());

        let m1 = make_movie("Alpha Movie", Some(2020));
        movie_repo.movies.lock().unwrap().insert(m1.id, m1);

        let s1 = Show {
            id: Uuid::new_v4(),
            title: "Beta Show".to_string(),
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        show_repo.shows.lock().unwrap().insert(s1.id, s1);

        let service = DbMetadataService::new(
            movie_repo,
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 2);
        assert!(!conn.page_info.has_next_page);
        // Sorted by title: Alpha Movie, Beta Show
        assert_eq!(conn.edges[0].node.title().original, "Alpha Movie");
        assert_eq!(conn.edges[1].node.title().original, "Beta Show");
    }

    #[tokio::test]
    async fn test_search_media_type_movie_filter() {
        use crate::services::metadata::MediaTypeFilter;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());

        let m1 = make_movie("Movie One", None);
        movie_repo.movies.lock().unwrap().insert(m1.id, m1);

        let s1 = Show {
            id: Uuid::new_v4(),
            title: "Show One".to_string(),
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        show_repo.shows.lock().unwrap().insert(s1.id, s1);

        let service = DbMetadataService::new(
            movie_repo,
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: Some(MediaTypeFilter::Movie),
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 1);
        assert_eq!(conn.edges[0].node.title().original, "Movie One");
    }

    #[tokio::test]
    async fn test_search_media_query_filter() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());

        let m1 = make_movie("Blade Runner", None);
        let m2 = make_movie("The Matrix", None);
        movie_repo.movies.lock().unwrap().insert(m1.id, m1);
        movie_repo.movies.lock().unwrap().insert(m2.id, m2);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: Some("blade".to_string()),
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 1);
        assert_eq!(conn.edges[0].node.title().original, "Blade Runner");
    }

    #[tokio::test]
    async fn test_search_media_year_filter() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());

        let m1 = make_movie("Movie 2020", Some(2020));
        let m2 = make_movie("Movie 2021", Some(2021));
        let m3 = make_movie("Movie 2022", Some(2022));
        movie_repo.movies.lock().unwrap().insert(m1.id, m1);
        movie_repo.movies.lock().unwrap().insert(m2.id, m2);
        movie_repo.movies.lock().unwrap().insert(m3.id, m3);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Year,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: Some(2021),
                    year_to: Some(2021),
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 1);
        assert_eq!(conn.edges[0].node.title().original, "Movie 2021");
    }

    #[tokio::test]
    async fn test_search_media_pagination() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());

        // Insert 3 movies in alphabetical order
        for title in &["Alpha", "Beta", "Gamma"] {
            let m = make_movie(title, None);
            movie_repo.movies.lock().unwrap().insert(m.id, m);
        }

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        // First page of 2
        let page1 = service
            .search_media(
                Some(2),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(page1.edges.len(), 2);
        assert!(page1.page_info.has_next_page);
        assert!(!page1.page_info.has_previous_page);
        let cursor = page1.page_info.end_cursor.unwrap();

        // Second page after cursor
        let page2 = service
            .search_media(
                Some(2),
                Some(cursor),
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(page2.edges.len(), 1);
        assert!(!page2.page_info.has_next_page);
        assert!(page2.page_info.has_previous_page);
        assert_eq!(page2.edges[0].node.title().original, "Gamma");
    }

    #[tokio::test]
    async fn test_search_media_sort_desc() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());

        for title in &["Alpha", "Beta", "Gamma"] {
            let m = make_movie(title, None);
            movie_repo.movies.lock().unwrap().insert(m.id, m);
        }

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Desc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 3);
        assert_eq!(conn.edges[0].node.title().original, "Gamma");
        assert_eq!(conn.edges[2].node.title().original, "Alpha");
    }

    #[tokio::test]
    async fn test_refresh_metadata_is_ok() {
        let service = make_service();
        let result = service.refresh_metadata(MediaFilter::All).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_empty_db_returns_empty() {
        let service = make_service();
        let conn = service
            .search_media(
                Some(10),
                None,
                None,
                None,
                MediaSortField::Title,
                SortOrder::Asc,
                MediaSearchFilters {
                    media_type: None,
                    genre: None,
                    year: None,
                    year_from: None,
                    year_to: None,
                    query: None,
                    min_rating: None,
                },
            )
            .await;

        assert_eq!(conn.edges.len(), 0);
        assert!(!conn.page_info.has_next_page);
        assert!(!conn.page_info.has_previous_page);
    }

    #[tokio::test]
    async fn test_get_media_sources_unknown_id_returns_not_found() {
        use crate::services::metadata::MetadataError;

        let service = make_service();
        let result = service.get_media_sources(&Uuid::new_v4().to_string()).await;
        assert!(matches!(result, Err(MetadataError::MediaNotFound)));
    }

    #[tokio::test]
    async fn test_get_media_sources_show_id_returns_unsupported() {
        use crate::services::metadata::MetadataError;

        let show_repo = Arc::new(InMemoryShowRepository::default());
        let show = Show {
            id: Uuid::new_v4(),
            title: "Test Show".to_string(),
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let show_id = show.id;
        show_repo.shows.lock().unwrap().insert(show.id, show);

        let service = DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let result = service.get_media_sources(&show_id.to_string()).await;
        assert!(matches!(result, Err(MetadataError::Unsupported(_))));
    }

    #[tokio::test]
    async fn test_get_media_sources_returns_movie_files_across_entries() {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());

        let movie = make_movie("Test Movie", Some(2023));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);

        let library_id = Uuid::new_v4();
        let entry = MovieEntry {
            id: Uuid::new_v4(),
            library_id,
            movie_id,
            edition: None,
            is_primary: true,
            created_at: chrono::Utc::now(),
        };
        let entry_id = entry.id;
        movie_repo.entries.lock().unwrap().insert(entry.id, entry);

        let file = make_media_file(
            library_id,
            MediaFileContent::Movie {
                movie_entry_id: entry_id,
            },
        );
        let file_id = file.id;
        file_repo.files.lock().unwrap().insert(file.id, file);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            stream_repo,
        );

        let sources = service
            .get_media_sources(&movie_id.to_string())
            .await
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].file_id, file_id.to_string());
        assert_eq!(sources[0].stream_url, format!("/v1/files/{file_id}/stream"));
        assert_eq!(
            sources[0].download_url,
            format!("/v1/files/{file_id}/download")
        );
        assert_eq!(sources[0].size_bytes, 1024);
    }

    /// Seeds a show/season/episode and returns the episode id, so the sources
    /// tests can drive the episode branch of `get_media_sources`.
    fn seed_episode(show_repo: &InMemoryShowRepository) -> Uuid {
        let show = Show {
            id: Uuid::new_v4(),
            title: "Test Show".to_string(),
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let show_id = show.id;
        show_repo.shows.lock().unwrap().insert(show.id, show);

        let season = Season {
            id: Uuid::new_v4(),
            show_id,
            season_number: 1,
            poster_url: None,
            first_aired: None,
            last_aired: None,
        };
        let season_id = season.id;
        show_repo.seasons.lock().unwrap().insert(season.id, season);

        let ep = Episode {
            id: Uuid::new_v4(),
            season_id,
            episode_number: 1,
            title: "Pilot".to_string(),
            description: None,
            air_date: None,
            runtime: None,
            thumbnail_url: None,
            created_at: chrono::Utc::now(),
        };
        let episode_id = ep.id;
        show_repo.episodes.lock().unwrap().insert(ep.id, ep);
        episode_id
    }

    #[tokio::test]
    async fn test_get_media_sources_returns_files_for_episode() {
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());

        let episode_id = seed_episode(&show_repo);

        // Two renditions for the same episode.
        let library_id = Uuid::new_v4();
        let file_a = make_media_file(library_id, MediaFileContent::Episode { episode_id });
        let file_b = make_media_file(library_id, MediaFileContent::Episode { episode_id });
        let file_a_id = file_a.id;
        let file_b_id = file_b.id;
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        let service = DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let mut sources = service
            .get_media_sources(&episode_id.to_string())
            .await
            .unwrap();
        assert_eq!(sources.len(), 2);

        // Order is not guaranteed by the in-memory HashMap; sort by file id so
        // the mapping assertions are deterministic.
        sources.sort_by(|a, b| a.file_id.cmp(&b.file_id));
        let mut expected = [file_a_id, file_b_id];
        expected.sort();

        for (source, id) in sources.iter().zip(expected.iter()) {
            assert_eq!(source.file_id, id.to_string());
            assert_eq!(source.stream_url, format!("/v1/files/{id}/stream"));
            assert_eq!(source.download_url, format!("/v1/files/{id}/download"));
        }
    }

    #[tokio::test]
    async fn test_get_media_sources_episode_without_files_returns_empty() {
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let episode_id = seed_episode(&show_repo);

        let service = DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        // A known episode with no files is playable-but-empty, not a 404.
        let sources = service
            .get_media_sources(&episode_id.to_string())
            .await
            .unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_stream_metadata_builder_reports_probed_codecs() {
        use crate::models::{OutputAudioCodec, OutputVideoCodec};
        use crate::services::metadata::build_media_stream_metadata_from_domain_streams;
        use beam_domain::models::stream::{
            AudioStreamMetadata, MediaStream, StreamMetadata, StreamType, VideoStreamMetadata,
        };

        let file_id = Uuid::new_v4();
        let video_stream = |codec: &str| MediaStream {
            id: Uuid::new_v4(),
            file_id,
            index: 0,
            stream_type: StreamType::Video,
            codec: codec.to_string(),
            metadata: StreamMetadata::Video(VideoStreamMetadata {
                width: 1920,
                height: 1080,
                frame_rate: Some(23.976),
                bit_rate: Some(8_000_000),
                color_space: None,
                color_range: None,
                hdr_format: None,
            }),
        };
        let audio_stream = |codec: &str| MediaStream {
            id: Uuid::new_v4(),
            file_id,
            index: 1,
            stream_type: StreamType::Audio,
            codec: codec.to_string(),
            metadata: StreamMetadata::Audio(AudioStreamMetadata {
                language: Some("eng".to_string()),
                title: None,
                channels: 6,
                sample_rate: 48_000,
                channel_layout: Some("5.1".to_string()),
                bit_rate: Some(640_000),
                is_default: true,
                is_forced: false,
            }),
        };

        let metadata = build_media_stream_metadata_from_domain_streams(&[
            video_stream("hevc"),
            video_stream("mpeg2video"),
            audio_stream("opus"),
            audio_stream("ac3"),
        ]);

        assert_eq!(metadata.video_tracks.len(), 2);
        assert_eq!(metadata.video_tracks[0].codec, OutputVideoCodec::H265);
        assert_eq!(metadata.video_tracks[1].codec, OutputVideoCodec::UNKNOWN);
        assert_eq!(metadata.audio_tracks.len(), 2);
        assert_eq!(metadata.audio_tracks[0].codec, OutputAudioCodec::Opus);
        assert_eq!(metadata.audio_tracks[1].codec, OutputAudioCodec::Unknown);
    }

    // ---------------------------------------------------------------------------
    // Artwork addressing (ADR-0015)
    // ---------------------------------------------------------------------------

    /// The privacy claim, asserted rather than assumed: what a client is handed
    /// must not be a provider URL, because a browser that is handed one fetches
    /// it and TMDB learns who is browsing what.
    #[tokio::test]
    async fn a_movie_points_at_beam_for_its_artwork_not_at_the_provider() {
        use crate::models::MediaMetadata;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let mut movie = make_movie("Arrival", Some(2016));
        movie.poster_url = Some("https://image.tmdb.org/t/p/w500/poster.jpg".to_string());
        movie.backdrop_url = Some("https://image.tmdb.org/t/p/w1280/backdrop.jpg".to_string());
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Movie(movie)) =
            service.get_media_metadata(&movie_id.to_string()).await
        else {
            panic!("the movie resolves");
        };

        assert_eq!(
            movie.poster_url.as_deref(),
            Some(format!("/v1/artwork/movie/{movie_id}/poster").as_str()),
        );
        assert_eq!(
            movie.backdrop_url.as_deref(),
            Some(format!("/v1/artwork/movie/{movie_id}/backdrop").as_str()),
        );
        for url in [movie.poster_url, movie.backdrop_url].into_iter().flatten() {
            assert!(
                !url.contains("tmdb.org"),
                "a provider URL reached the client: {url}",
            );
        }
    }

    /// A title with no art must stay `None` rather than becoming a link that
    /// every client dutifully requests and every request 404s.
    #[tokio::test]
    async fn a_movie_with_no_artwork_is_given_no_artwork_url() {
        use crate::models::MediaMetadata;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let movie = make_movie("Un-enriched", None);
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);

        let service = DbMetadataService::new(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Movie(movie)) =
            service.get_media_metadata(&movie_id.to_string()).await
        else {
            panic!("the movie resolves");
        };
        assert_eq!(movie.poster_url, None);
        assert_eq!(movie.backdrop_url, None);
    }

    /// Season posters and episode stills are addressed by their own rows, which
    /// is why a season carries its id: `beam-web` falls back to a season poster
    /// whenever a show has none, so without the id that fallback has nothing to
    /// build a URL from.
    #[tokio::test]
    async fn seasons_and_episodes_address_their_own_artwork() {
        use crate::models::MediaMetadata;

        let show_repo = Arc::new(InMemoryShowRepository::default());
        let show = Show {
            id: Uuid::new_v4(),
            title: "Severance".to_string(),
            title_localized: None,
            description: None,
            year: Some(2022),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let show_id = show.id;
        show_repo.shows.lock().unwrap().insert(show.id, show);

        let season = Season {
            id: Uuid::new_v4(),
            show_id,
            season_number: 1,
            poster_url: Some("https://image.tmdb.org/t/p/w500/season.jpg".to_string()),
            first_aired: None,
            last_aired: None,
        };
        let season_id = season.id;
        show_repo.seasons.lock().unwrap().insert(season.id, season);

        let episode = Episode {
            id: Uuid::new_v4(),
            season_id,
            episode_number: 1,
            title: "Good News About Hell".to_string(),
            description: None,
            air_date: None,
            runtime: None,
            thumbnail_url: Some("https://image.tmdb.org/t/p/w300/still.jpg".to_string()),
            created_at: chrono::Utc::now(),
        };
        let episode_id = episode.id;
        show_repo
            .episodes
            .lock()
            .unwrap()
            .insert(episode.id, episode);

        let service = DbMetadataService::new(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Show(show)) =
            service.get_media_metadata(&show_id.to_string()).await
        else {
            panic!("the show resolves");
        };
        let season = show.seasons.first().expect("one season");

        assert_eq!(season.id, season_id.to_string());
        assert_eq!(
            season.poster_url.as_deref(),
            Some(format!("/v1/artwork/season/{season_id}/poster").as_str()),
        );
        assert_eq!(
            season.episodes[0].thumbnail_url.as_deref(),
            Some(format!("/v1/artwork/episode/{episode_id}/thumbnail").as_str()),
        );
    }
}
