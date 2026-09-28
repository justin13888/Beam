//! Tests for DbMetadataService using in-memory repository fakes.
//!
//! These tests exercise the full metadata service vertical slice without any
//! external infrastructure. All repositories are stateful in-memory fakes.

/// Sources over empty stores, for a service whose test reads none.
#[cfg(test)]
fn empty_sources() -> std::sync::Arc<crate::services::sources::SourceCatalog> {
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use std::sync::Arc;

    Arc::new(crate::services::sources::SourceCatalog::new(
        Arc::new(InMemoryMovieRepository::default()),
        Arc::new(InMemoryFileRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(InMemorySidecarSubtitleRepository::default()),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use crate::services::metadata::{
        BrowseRequest, DbMetadataService, MediaSearchFilters, MediaSortField, MetadataRepositories,
        MetadataService, SortOrder,
    };
    use beam_domain::models::movie::Movie;
    use beam_domain::models::{Episode, MediaFile, MediaFileContent, MovieEntry, Season, Show};
    use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

    use crate::services::sources::SourceCatalog;

    // ---------------------------------------------------------------------------
    // Helper builders
    // ---------------------------------------------------------------------------

    fn make_movie(title: &str, year: Option<u32>) -> Movie {
        Movie {
            id: Uuid::new_v4(),
            title: title.to_string(),
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
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
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: Some(Duration::from_secs(7200)),
            container_format: Some("mp4".to_string()),
            content: Some(content),
            status: beam_domain::models::FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    /// The real service over the given doubles, with a catalogue and a genre
    /// store reading the same title doubles.
    fn service(
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        files: Arc<InMemoryFileRepository>,
        streams: Arc<InMemoryMediaStreamRepository>,
    ) -> DbMetadataService {
        service_with_genres(movies, shows, files, streams, Arc::default())
    }

    fn service_with_genres(
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        files: Arc<InMemoryFileRepository>,
        streams: Arc<InMemoryMediaStreamRepository>,
        genres: Arc<InMemoryGenreRepository>,
    ) -> DbMetadataService {
        service_with_sidecars(movies, shows, files, streams, Arc::default(), genres)
    }

    fn service_with_sidecars(
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        files: Arc<InMemoryFileRepository>,
        streams: Arc<InMemoryMediaStreamRepository>,
        sidecars: Arc<InMemorySidecarSubtitleRepository>,
        genres: Arc<InMemoryGenreRepository>,
    ) -> DbMetadataService {
        DbMetadataService::new(MetadataRepositories {
            catalog: Arc::new(InMemoryCatalogRepository::new(
                movies.clone(),
                shows.clone(),
                genres.clone(),
            )),
            sources: Arc::new(SourceCatalog::new(movies.clone(), files, streams, sidecars)),
            movies,
            shows,
            genres,
        })
    }

    fn make_service() -> DbMetadataService {
        service(
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
        let result = service.get_media_metadata(Uuid::new_v4()).await;
        assert!(matches!(result, Ok(None)), "{result:?}");
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

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let result = service
            .get_media_metadata(movie_id)
            .await
            .expect("the lookup succeeds");
        match result.expect("the title resolves") {
            MediaMetadata::Movie(m) => {
                assert_eq!(m.id, movie_id);
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
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: Some("A test show".to_string()),
            year: Some(2022),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
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

        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let result = service
            .get_media_metadata(show_id)
            .await
            .expect("the lookup succeeds");
        match result.expect("the title resolves") {
            MediaMetadata::Show(s) => {
                assert_eq!(s.id, show_id);
                assert_eq!(s.title.original, "Test Show");
                assert_eq!(s.year, Some(2022));
                assert_eq!(s.seasons.len(), 1);
                assert_eq!(s.seasons[0].episodes.len(), 1);
                let episode = &s.seasons[0].episodes[0];
                assert_eq!(episode.id, episode_id);
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
    async fn test_get_media_sources_unknown_id_returns_not_found() {
        use crate::services::metadata::MetadataError;

        let service = make_service();
        let result = service.get_media_sources(&Uuid::new_v4().to_string()).await;
        assert!(matches!(result, Err(MetadataError::MediaNotFound)));
    }

    /// A malformed id is the caller's mistake, and must stay distinguishable
    /// from both a well-formed miss and a server fault.
    ///
    /// It was folded into `InternalError`, which the routes render as a 500 --
    /// so `/v1/media/{id}/sources` answered a typo with "the server broke"
    /// while `/v1/media/{id}` answered the same typo with 404 (issue #123).
    #[tokio::test]
    async fn test_get_media_sources_malformed_id_is_distinct_from_a_miss() {
        use crate::services::metadata::MetadataError;

        let service = make_service();

        let malformed = service.get_media_sources("not-a-uuid").await;
        assert!(
            matches!(malformed, Err(MetadataError::InvalidId)),
            "a malformed id must be InvalidId, got {malformed:?}"
        );

        let missing = service.get_media_sources(&Uuid::new_v4().to_string()).await;
        assert!(
            matches!(missing, Err(MetadataError::MediaNotFound)),
            "a well-formed id that resolves to nothing stays MediaNotFound"
        );
    }

    #[tokio::test]
    async fn test_get_media_sources_show_id_returns_unsupported() {
        use crate::services::metadata::MetadataError;

        let show_repo = Arc::new(InMemoryShowRepository::default());
        let show = Show {
            id: Uuid::new_v4(),
            title: "Test Show".to_string(),
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let show_id = show.id;
        show_repo.shows.lock().unwrap().insert(show.id, show);

        let service = service(
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

        let service = service(
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
        assert_eq!(sources[0].file_id, file_id);
        assert!(sources[0].is_primary);
        assert_eq!(sources[0].stream_url, format!("/v1/files/{file_id}/stream"));
        assert_eq!(
            sources[0].download_url,
            format!("/v1/files/{file_id}/download")
        );
        assert_eq!(sources[0].size_bytes, 1024);
    }

    #[tokio::test]
    async fn test_get_media_sources_omits_a_file_that_went_missing() {
        // A source that is not on disk cannot be streamed, so it is not offered
        // (issue #179); the rendition still on disk is.
        use beam_domain::repositories::FileRepository;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());

        let movie = make_movie("Two Cuts", Some(2020));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);
        let library_id = Uuid::new_v4();
        let entry = MovieEntry {
            id: Uuid::new_v4(),
            library_id,
            movie_id,
            edition: None,
            created_at: chrono::Utc::now(),
        };
        let content = MediaFileContent::Movie {
            movie_entry_id: entry.id,
        };
        movie_repo.entries.lock().unwrap().insert(entry.id, entry);
        let present = make_media_file(library_id, content.clone());
        let missing = make_media_file(library_id, content);
        let present_id = present.id;
        let missing_id = missing.id;
        for file in [present, missing] {
            file_repo.files.lock().unwrap().insert(file.id, file);
        }
        file_repo
            .mark_missing(vec![missing_id], chrono::Utc::now())
            .await
            .unwrap();

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let sources = service
            .get_media_sources(&movie_id.to_string())
            .await
            .unwrap();
        let ids: Vec<Uuid> = sources.iter().map(|s| s.file_id).collect();
        assert_eq!(ids, vec![present_id]);
    }

    /// Seeds a show/season/episode and returns the episode id, so the sources
    /// tests can drive the episode branch of `get_media_sources`.
    fn seed_episode(show_repo: &InMemoryShowRepository) -> Uuid {
        let show = Show {
            id: Uuid::new_v4(),
            title: "Test Show".to_string(),
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
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
        let file_a = make_media_file(library_id, MediaFileContent::episode(episode_id));
        let file_b = make_media_file(library_id, MediaFileContent::episode(episode_id));
        let file_a_id = file_a.id;
        let file_b_id = file_b.id;
        file_repo.files.lock().unwrap().insert(file_a.id, file_a);
        file_repo.files.lock().unwrap().insert(file_b.id, file_b);

        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let sources = service
            .get_media_sources(&episode_id.to_string())
            .await
            .unwrap();
        assert_eq!(sources.len(), 2);

        // Two files alike in every other way are ranked by id, whatever
        // order the store returned them in.
        let mut expected = [file_a_id, file_b_id];
        expected.sort();

        for (source, id) in sources.iter().zip(expected.iter()) {
            assert_eq!(source.file_id, *id);
            assert_eq!(source.stream_url, format!("/v1/files/{id}/stream"));
            assert_eq!(source.download_url, format!("/v1/files/{id}/download"));
        }
    }

    #[tokio::test]
    async fn test_get_media_sources_episode_without_files_returns_empty() {
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let episode_id = seed_episode(&show_repo);

        let service = service(
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

    // ---------------------------------------------------------------------------
    // Sources: one track model, ranked (issue #189)
    // ---------------------------------------------------------------------------

    /// A video stream `height` lines tall at `bit_rate`, as the indexer
    /// records one.
    fn video_stream(
        file_id: Uuid,
        index: u32,
        codec: &str,
        height: u32,
        bit_rate: Option<u64>,
        frame_rate: Option<f64>,
    ) -> beam_domain::models::CreateMediaStream {
        use beam_domain::models::stream::{StreamMetadata, StreamType, VideoStreamMetadata};
        beam_domain::models::CreateMediaStream {
            file_id,
            index,
            stream_type: StreamType::Video,
            codec: codec.to_string(),
            metadata: StreamMetadata::Video(VideoStreamMetadata {
                width: height * 16 / 9,
                height,
                frame_rate,
                bit_rate,
                color_space: None,
                color_range: None,
                hdr_format: None,
            }),
        }
    }

    fn audio_stream(
        file_id: Uuid,
        index: u32,
        codec: &str,
        sample_rate: u32,
        is_default: bool,
    ) -> beam_domain::models::CreateMediaStream {
        use beam_domain::models::stream::{AudioStreamMetadata, StreamMetadata, StreamType};
        beam_domain::models::CreateMediaStream {
            file_id,
            index,
            stream_type: StreamType::Audio,
            codec: codec.to_string(),
            metadata: StreamMetadata::Audio(AudioStreamMetadata {
                language: Some("eng".to_string()),
                title: None,
                channels: 8,
                sample_rate,
                channel_layout: Some("7.1".to_string()),
                bit_rate: None,
                is_default,
                is_forced: false,
            }),
        }
    }

    fn subtitle_stream(
        file_id: Uuid,
        index: u32,
        codec: &str,
        is_hearing_impaired: bool,
    ) -> beam_domain::models::CreateMediaStream {
        use beam_domain::models::stream::{StreamMetadata, StreamType, SubtitleStreamMetadata};
        beam_domain::models::CreateMediaStream {
            file_id,
            index,
            stream_type: StreamType::Subtitle,
            codec: codec.to_string(),
            metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
                language: Some("eng".to_string()),
                title: None,
                is_default: false,
                is_forced: false,
                is_hearing_impaired,
            }),
        }
    }

    /// A movie entry of `edition` for `movie_id`.
    fn entry(movie_repo: &InMemoryMovieRepository, movie_id: Uuid, edition: Option<&str>) -> Uuid {
        let entry = MovieEntry {
            id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            movie_id,
            edition: edition.map(str::to_string),
            created_at: chrono::Utc::now(),
        };
        let id = entry.id;
        movie_repo.entries.lock().unwrap().insert(entry.id, entry);
        id
    }

    /// A file of `content` of `size_bytes` lasting `duration_secs`.
    fn file_of(
        file_repo: &InMemoryFileRepository,
        content: MediaFileContent,
        size_bytes: u64,
        duration_secs: u64,
    ) -> Uuid {
        let mut file = make_media_file(Uuid::new_v4(), content);
        file.size_bytes = size_bytes;
        file.duration = Some(Duration::from_secs(duration_secs));
        let id = file.id;
        file_repo.files.lock().unwrap().insert(file.id, file);
        id
    }

    /// The primary is chosen from what the files are, not the order they
    /// were found: the default edition over a named one however good, then
    /// the taller picture over a larger file. The detail route's `file_id`
    /// and duration are the primary's, and `/sources` lists it first.
    #[tokio::test]
    async fn the_tallest_default_edition_file_is_primary_and_listed_first() {
        use crate::models::MediaMetadata;
        use beam_domain::repositories::MediaStreamRepository;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let movie = make_movie("Heat", Some(1995));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);

        let theatrical = entry(&movie_repo, movie_id, None);
        let directors_cut = entry(&movie_repo, movie_id, Some("Director's Cut"));
        let content = |movie_entry_id| MediaFileContent::Movie { movie_entry_id };
        // The 720p file is the largest; the Director's Cut is the tallest
        // and the highest bit rate.
        let hd = file_of(&file_repo, content(theatrical), 9_000, 100);
        let uhd = file_of(&file_repo, content(theatrical), 5_000, 200);
        let cut = file_of(&file_repo, content(directors_cut), 9_999, 300);
        stream_repo
            .insert_streams(vec![
                video_stream(hd, 0, "h264", 720, Some(4_000_000), Some(23.976)),
                video_stream(uhd, 0, "hevc", 2160, Some(20_000_000), Some(23.976)),
                video_stream(cut, 0, "hevc", 2160, Some(60_000_000), Some(23.976)),
            ])
            .await
            .unwrap();

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            stream_repo,
        );

        let sources = service
            .get_media_sources(&movie_id.to_string())
            .await
            .unwrap();
        let order: Vec<(Uuid, bool, Option<&str>)> = sources
            .iter()
            .map(|s| (s.file_id, s.is_primary, s.edition.as_deref()))
            .collect();
        assert_eq!(
            order,
            vec![
                (uhd, true, None),
                (hd, false, None),
                (cut, false, Some("Director's Cut")),
            ]
        );

        let Some(MediaMetadata::Movie(detail)) =
            service.get_media_metadata(movie_id).await.unwrap()
        else {
            panic!("the movie resolves");
        };
        assert_eq!(detail.file_id, Some(uhd));
        assert_eq!(detail.duration, Some(200.0));
        assert_eq!(detail.source_count, Some(3));
    }

    /// Every track is addressed by its stream index and names its codec as
    /// FFmpeg does, whatever the codec: E-AC-3 and TrueHD are no longer
    /// `Unknown`, a PGS subtitle is not `WebVTT`. A value the file does not
    /// state is absent, never a made-up default.
    #[tokio::test]
    async fn tracks_carry_their_index_and_real_codec() {
        use crate::models::SubtitleOrigin;
        use beam_domain::repositories::MediaStreamRepository;

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let movie = make_movie("Dune", Some(2021));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);
        let file = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: entry(&movie_repo, movie_id, None),
            },
            1,
            1,
        );
        stream_repo
            .insert_streams(vec![
                subtitle_stream(file, 4, "subrip", true),
                audio_stream(file, 2, "truehd", 48_000, true),
                video_stream(file, 0, "hevc", 2160, Some(0), None),
                audio_stream(file, 1, "eac3", 0, false),
                subtitle_stream(file, 3, "hdmv_pgs_subtitle", false),
            ])
            .await
            .unwrap();

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            stream_repo,
        );
        let sources = service
            .get_media_sources(&movie_id.to_string())
            .await
            .unwrap();
        let source = &sources[0];

        let video = &source.video_tracks[0];
        assert_eq!((video.index, video.codec.as_str()), (0, "hevc"));
        assert_eq!(video.frame_rate, None, "no 29.97 is invented");
        assert_eq!(video.bit_rate, None, "a zero bit rate is an unknown one");

        let audio: Vec<(u32, &str, Option<u32>, bool)> = source
            .audio_tracks
            .iter()
            .map(|a| (a.index, a.codec.as_str(), a.sample_rate, a.is_default))
            .collect();
        assert_eq!(
            audio,
            vec![(1, "eac3", None, false), (2, "truehd", Some(48_000), true)]
        );

        let subtitles: Vec<_> = source
            .subtitle_tracks
            .iter()
            .map(|s| {
                (
                    s.origin,
                    s.index,
                    s.codec.as_str(),
                    s.is_text,
                    s.is_hearing_impaired,
                    s.url.is_some(),
                )
            })
            .collect();
        assert_eq!(
            subtitles,
            vec![
                (
                    SubtitleOrigin::Embedded,
                    Some(3),
                    "hdmv_pgs_subtitle",
                    false,
                    false,
                    false
                ),
                (
                    SubtitleOrigin::Embedded,
                    Some(4),
                    "subrip",
                    true,
                    true,
                    false
                ),
            ],
            "an embedded track is never served on its own"
        );
    }

    /// The subtitle files beside a video are its tracks too: after the
    /// embedded ones, each with where to fetch it, and a WebVTT rendition
    /// exactly for a SubRip or WebVTT file small enough to convert. Another
    /// file's subtitles are not this one's.
    #[tokio::test]
    async fn sidecar_subtitles_follow_the_embedded_tracks_with_their_urls() {
        use crate::models::SubtitleOrigin;
        use crate::services::sources::SUBTITLE_CONVERT_MAX_BYTES;
        use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
        use beam_domain::repositories::{MediaStreamRepository, SidecarSubtitleRepository};

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
        let movie = make_movie("Alien", Some(1979));
        let movie_id = movie.id;
        movie_repo.movies.lock().unwrap().insert(movie.id, movie);
        let content = MediaFileContent::Movie {
            movie_entry_id: entry(&movie_repo, movie_id, None),
        };
        // The other file is the smaller, so this one is primary and first.
        let file = file_of(&file_repo, content.clone(), 2, 1);
        let other = file_of(&file_repo, content, 1, 1);
        stream_repo
            .insert_streams(vec![subtitle_stream(file, 2, "subrip", false)])
            .await
            .unwrap();

        let sidecar = |file_id, name: &str, format, language: Option<&str>, forced, size| {
            UpsertSidecarSubtitle {
                file_id,
                library_id: Uuid::new_v4(),
                path: std::path::PathBuf::from(format!("/videos/{name}")),
                info: SidecarInfo {
                    format,
                    language: language.map(str::to_string),
                    title: None,
                    is_forced: forced,
                    is_sdh: name.contains("sdh"),
                    is_default: false,
                },
                size_bytes: size,
                mtime: None,
            }
        };
        let mut ids = std::collections::HashMap::new();
        for upsert in [
            sidecar(file, "Alien.srt", SubtitleFormat::Srt, None, false, 10),
            sidecar(
                file,
                "Alien.fre.ass",
                SubtitleFormat::Ass,
                Some("fre"),
                false,
                10,
            ),
            sidecar(
                file,
                "Alien.eng.forced.srt",
                SubtitleFormat::Srt,
                Some("eng"),
                true,
                10,
            ),
            sidecar(
                file,
                "Alien.eng.sdh.vtt",
                SubtitleFormat::Vtt,
                Some("eng"),
                false,
                10,
            ),
            sidecar(
                file,
                "Alien.spa.srt",
                SubtitleFormat::Srt,
                Some("spa"),
                false,
                SUBTITLE_CONVERT_MAX_BYTES + 1,
            ),
            sidecar(
                other,
                "Other.eng.srt",
                SubtitleFormat::Srt,
                Some("eng"),
                false,
                10,
            ),
        ] {
            let name = upsert
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let row = sidecar_repo.upsert_by_path(upsert).await.unwrap();
            ids.insert(name, row.id);
        }

        let service = service_with_sidecars(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            stream_repo,
            sidecar_repo,
            Arc::default(),
        );
        let sources = service
            .get_media_sources(&movie_id.to_string())
            .await
            .unwrap();
        assert_eq!(sources[0].file_id, file);
        let tracks = &sources[0].subtitle_tracks;

        assert_eq!(tracks[0].origin, SubtitleOrigin::Embedded);
        let sidecars: Vec<(&str, &str, bool, bool, bool)> = tracks[1..]
            .iter()
            .map(|track| {
                let id = track.sidecar_id.expect("a sidecar has an id");
                let name = ids
                    .iter()
                    .find_map(|(name, row)| (*row == id).then_some(name.as_str()))
                    .expect("a sidecar of this file");
                assert_eq!(track.origin, SubtitleOrigin::Sidecar);
                assert_eq!(track.index, None);
                assert!(track.is_text);
                assert_eq!(
                    track.url.as_deref(),
                    Some(format!("/v1/files/{file}/subtitles/{id}").as_str())
                );
                if let Some(webvtt) = &track.webvtt_url {
                    assert_eq!(webvtt, &format!("/v1/files/{file}/subtitles/{id}/webvtt"));
                }
                (
                    name,
                    track.codec.as_str(),
                    track.is_forced,
                    track.is_hearing_impaired,
                    track.webvtt_url.is_some(),
                )
            })
            .collect();
        assert_eq!(
            sidecars,
            vec![
                ("Alien.eng.sdh.vtt", "webvtt", false, true, true),
                ("Alien.eng.forced.srt", "subrip", true, false, true),
                ("Alien.fre.ass", "ass", false, false, false),
                ("Alien.spa.srt", "subrip", false, false, false),
                ("Alien.srt", "subrip", false, false, true),
            ],
            "by language, untagged last, full before forced; ASS and oversized files as stored only"
        );
    }

    /// A file holding a run of episodes says so on its source, and its
    /// duration -- the whole run's -- is not given as the episode's.
    #[tokio::test]
    async fn an_episode_file_holding_a_run_says_so_and_lends_no_duration() {
        use crate::models::MediaMetadata;

        let show_repo = Arc::new(InMemoryShowRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let episode_id = seed_episode(&show_repo);
        let show_id = *show_repo.shows.lock().unwrap().keys().next().unwrap();
        file_of(
            &file_repo,
            MediaFileContent::Episode {
                episode_id,
                last_episode_number: Some(3),
            },
            1,
            3 * 45 * 60,
        );

        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let sources = service
            .get_media_sources(&episode_id.to_string())
            .await
            .unwrap();
        let span = sources[0].episode_span.expect("the file spans episodes");
        assert_eq!(
            (span.first_episode_number, span.last_episode_number),
            (1, 3)
        );
        assert_eq!(sources[0].duration_secs, Some(8100.0));

        let Some(MediaMetadata::Show(show)) = service.get_media_metadata(show_id).await.unwrap()
        else {
            panic!("the show resolves");
        };
        let episode = &show.seasons[0].episodes[0];
        assert_eq!(episode.file_id, Some(sources[0].file_id));
        assert_eq!(episode.duration, None);
        assert_eq!(episode.source_count, 1);
    }

    /// A single-episode file spans nothing and lends the episode its
    /// duration.
    #[tokio::test]
    async fn a_single_episode_file_lends_its_duration() {
        use crate::models::MediaMetadata;

        let show_repo = Arc::new(InMemoryShowRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let episode_id = seed_episode(&show_repo);
        let show_id = *show_repo.shows.lock().unwrap().keys().next().unwrap();
        file_of(
            &file_repo,
            MediaFileContent::episode(episode_id),
            1,
            45 * 60,
        );

        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            file_repo,
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let sources = service
            .get_media_sources(&episode_id.to_string())
            .await
            .unwrap();
        assert!(sources[0].episode_span.is_none());
        let Some(MediaMetadata::Show(show)) = service.get_media_metadata(show_id).await.unwrap()
        else {
            panic!("the show resolves");
        };
        assert_eq!(show.seasons[0].episodes[0].duration, Some(2700.0));
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

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Movie(movie)) = service
            .get_media_metadata(movie_id)
            .await
            .expect("the lookup succeeds")
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

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Movie(movie)) = service
            .get_media_metadata(movie_id)
            .await
            .expect("the lookup succeeds")
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
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year: Some(2022),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
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

        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let Some(MediaMetadata::Show(show)) = service
            .get_media_metadata(show_id)
            .await
            .expect("the lookup succeeds")
        else {
            panic!("the show resolves");
        };
        let season = show.seasons.first().expect("one season");

        assert_eq!(season.id, season_id);
        assert_eq!(
            season.poster_url.as_deref(),
            Some(format!("/v1/artwork/season/{season_id}/poster").as_str()),
        );
        assert_eq!(
            season.episodes[0].thumbnail_url.as_deref(),
            Some(format!("/v1/artwork/episode/{episode_id}/thumbnail").as_str()),
        );
    }

    /// Seeds a show through the repository trait -- created, then enriched
    /// with whatever artwork the provider supplied -- so the metadata service
    /// sees the same row shape enrichment leaves behind.
    async fn seed_enriched_show(
        show_repo: &InMemoryShowRepository,
        poster_url: Option<&str>,
        backdrop_url: Option<&str>,
    ) -> Uuid {
        use beam_domain::models::CreateShow;
        use beam_domain::providers::enrichment::ShowEnrichment;
        use beam_domain::repositories::ShowRepository;

        let show = show_repo
            .find_or_create_by_identity(CreateShow::new("Severance".to_string(), Some(2022)))
            .await
            .expect("in-memory create succeeds");
        show_repo
            .apply_enrichment(
                show.id,
                &ShowEnrichment {
                    title: "Severance".to_string(),
                    year: Some(2022),
                    poster_url: poster_url.map(str::to_string),
                    backdrop_url: backdrop_url.map(str::to_string),
                    ..ShowEnrichment::default()
                },
                &beam_domain::models::enrichment::FieldLocks::none(),
            )
            .await
            .expect("in-memory enrichment succeeds");
        show.id
    }

    fn only_shows() -> MediaSearchFilters {
        use crate::services::metadata::MediaTypeFilter;

        MediaSearchFilters {
            media_type: Some(MediaTypeFilter::Show),
            genre: None,
            year: None,
            year_from: None,
            year_to: None,
            query: None,
            min_rating: None,
        }
    }

    /// Returns the show metadata from the detail view and from browse, for a
    /// catalogue holding exactly the one show.
    async fn show_detail_and_browsed(
        service: &DbMetadataService,
        show_id: Uuid,
    ) -> (crate::models::ShowMetadata, crate::models::ShowMetadata) {
        use crate::models::MediaMetadata;

        let Some(MediaMetadata::Show(detail)) = service
            .get_media_metadata(show_id)
            .await
            .expect("the lookup succeeds")
        else {
            panic!("the show resolves");
        };
        let conn = service
            .search_media(BrowseRequest {
                first: Some(10),
                after: None,
                last: None,
                before: None,
                sort_by: MediaSortField::Title,
                sort_order: SortOrder::Asc,
                filters: only_shows(),
            })
            .await
            .expect("browse succeeds");
        let [item] = conn.items.as_slice() else {
            panic!("exactly the one seeded show is browsed");
        };
        let MediaMetadata::Show(browsed) = item else {
            panic!("the browsed item is a show");
        };
        (detail, browsed.clone())
    }

    /// A show's own artwork reaches the client -- on the detail view and in
    /// browse results alike -- as a Beam artwork path, never as the provider
    /// URL enrichment stored.
    #[tokio::test]
    async fn a_show_points_at_beam_for_its_own_artwork() {
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let show_id = seed_enriched_show(
            &show_repo,
            Some("https://image.tmdb.org/t/p/w500/show-poster.jpg"),
            Some("https://image.tmdb.org/t/p/w1280/show-backdrop.jpg"),
        )
        .await;
        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );
        let expected_poster = format!("/v1/artwork/show/{show_id}/poster");
        let expected_backdrop = format!("/v1/artwork/show/{show_id}/backdrop");

        let (detail, browsed) = show_detail_and_browsed(&service, show_id).await;

        for (view, show) in [("detail", &detail), ("browse", &browsed)] {
            assert_eq!(
                show.poster_url.as_deref(),
                Some(expected_poster.as_str()),
                "{view} poster",
            );
            assert_eq!(
                show.backdrop_url.as_deref(),
                Some(expected_backdrop.as_str()),
                "{view} backdrop",
            );
        }
    }

    /// A show with no art of its own carries no artwork URL, on detail or in
    /// browse, so clients fall back to a season poster rather than requesting
    /// a path that 404s.
    #[tokio::test]
    async fn a_show_with_no_artwork_is_given_no_artwork_url() {
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let show_id = seed_enriched_show(&show_repo, None, None).await;
        let service = service(
            Arc::new(InMemoryMovieRepository::default()),
            show_repo,
            Arc::new(InMemoryFileRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
        );

        let (detail, browsed) = show_detail_and_browsed(&service, show_id).await;

        for (view, show) in [("detail", &detail), ("browse", &browsed)] {
            assert_eq!(show.poster_url, None, "{view} poster");
            assert_eq!(show.backdrop_url, None, "{view} backdrop");
        }
    }
}

/// Browse and search through the service (issue #187): the page request rules,
/// cursors, hydration of genres, ratings and counts, and that a failing store
/// is an error rather than an empty page or a miss.
#[cfg(test)]
mod browse {
    use std::sync::Arc;

    use beam_domain::models::catalog::{CatalogPosition, SortKey, TitleKind};
    use beam_domain::models::{CreateEpisode, CreateMovie, CreateShow};
    use beam_domain::providers::enrichment::ShowEnrichment;
    use beam_domain::repositories::catalog::MockCatalogRepository;
    use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
    use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
    use beam_domain::repositories::movie::MockMovieRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::{
        CatalogRepository, GenreRepository, MovieRepository, ShowRepository,
    };
    use proptest::prelude::*;
    use sea_orm::DbErr;
    use uuid::Uuid;

    use crate::models::MediaMetadata;
    use crate::services::cursor;
    use crate::services::metadata::{
        BrowseRequest, DEFAULT_PAGE_SIZE, DbMetadataService, MAX_PAGE_SIZE, MediaConnection,
        MediaSearchFilters, MediaSortField, MediaTypeFilter, MetadataError, MetadataRepositories,
        MetadataService, PageDirection, PageRequest, SortOrder,
    };

    /// Every double, with the catalogue and genre store reading the title
    /// doubles, so a test writes through one trait and reads through another.
    struct Library {
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        genres: Arc<InMemoryGenreRepository>,
    }

    impl Library {
        fn new() -> Self {
            Self {
                movies: Arc::default(),
                shows: Arc::default(),
                genres: Arc::default(),
            }
        }

        fn service(&self) -> DbMetadataService {
            self.service_over(Arc::new(InMemoryCatalogRepository::new(
                self.movies.clone(),
                self.shows.clone(),
                self.genres.clone(),
            )))
        }

        fn service_over(&self, catalog: Arc<dyn CatalogRepository>) -> DbMetadataService {
            DbMetadataService::new(MetadataRepositories {
                movies: self.movies.clone(),
                shows: self.shows.clone(),
                sources: super::empty_sources(),
                catalog,
                genres: self.genres.clone(),
            })
        }

        async fn movie(&self, title: &str) -> Uuid {
            self.movies
                .find_or_create_by_identity(CreateMovie::new(title, None, None))
                .await
                .unwrap()
                .id
        }

        /// A rated, identified show with genres, two seasons and three
        /// episodes.
        async fn full_show(&self, title: &str) -> Uuid {
            let show = self
                .shows
                .find_or_create_by_identity(CreateShow::new(title, Some(2022)))
                .await
                .unwrap();
            self.shows
                .apply_enrichment(
                    show.id,
                    &ShowEnrichment {
                        title: title.to_string(),
                        year: Some(2022),
                        tmdb_id: Some(95396),
                        imdb_id: Some("tt11280740".to_string()),
                        rating: Some(8.4),
                        ..Default::default()
                    },
                    &beam_domain::models::enrichment::FieldLocks::none(),
                )
                .await
                .unwrap();
            for (season_number, episodes) in [(1, 2), (2, 1)] {
                let season = self
                    .shows
                    .find_or_create_season(show.id, season_number)
                    .await
                    .unwrap();
                for episode_number in 1..=episodes {
                    self.shows
                        .find_or_create_episode(CreateEpisode {
                            season_id: season.id,
                            episode_number,
                            title: format!("Episode {episode_number}"),
                            runtime: None,
                            air_date: None,
                        })
                        .await
                        .unwrap();
                }
            }
            self.genres
                .set_show_genres(show.id, &["Thriller".to_string(), "drama".to_string()])
                .await
                .unwrap();
            show.id
        }
    }

    fn request(first: Option<u32>, after: Option<String>) -> BrowseRequest {
        BrowseRequest {
            first,
            after,
            last: None,
            before: None,
            sort_by: MediaSortField::Title,
            sort_order: SortOrder::Asc,
            filters: MediaSearchFilters::default(),
        }
    }

    fn backward(last: Option<u32>, before: Option<String>) -> BrowseRequest {
        BrowseRequest {
            first: None,
            after: None,
            last,
            before,
            ..request(None, None)
        }
    }

    fn titles(connection: &MediaConnection) -> Vec<String> {
        connection
            .items
            .iter()
            .map(|item| item.title().original.clone())
            .collect()
    }

    #[test]
    fn page_requests_page_one_way_within_the_size_bounds() {
        let c = || Some("c".to_string());
        let ok = |direction, size: u32, cursor: Option<String>| {
            Ok::<_, ()>(PageRequest {
                direction,
                size: std::num::NonZeroU32::new(size).unwrap(),
                cursor,
            })
        };
        /// `first`, `after`, `last`, `before`, and the page they ask for.
        type Case = (
            Option<u32>,
            Option<String>,
            Option<u32>,
            Option<String>,
            Result<PageRequest, ()>,
        );
        let cases: Vec<Case> = vec![
            (
                None,
                None,
                None,
                None,
                ok(PageDirection::Forward, DEFAULT_PAGE_SIZE, None),
            ),
            (Some(5), c(), None, None, ok(PageDirection::Forward, 5, c())),
            (
                None,
                c(),
                None,
                None,
                ok(PageDirection::Forward, DEFAULT_PAGE_SIZE, c()),
            ),
            (
                None,
                None,
                Some(3),
                None,
                ok(PageDirection::Backward, 3, None),
            ),
            (
                None,
                None,
                None,
                c(),
                ok(PageDirection::Backward, DEFAULT_PAGE_SIZE, c()),
            ),
            (
                None,
                None,
                Some(1),
                c(),
                ok(PageDirection::Backward, 1, c()),
            ),
            (
                Some(MAX_PAGE_SIZE),
                None,
                None,
                None,
                ok(PageDirection::Forward, MAX_PAGE_SIZE, None),
            ),
            (Some(MAX_PAGE_SIZE + 1), None, None, None, Err(())),
            (None, None, Some(MAX_PAGE_SIZE + 1), None, Err(())),
            (Some(0), None, None, None, Err(())),
            (None, None, Some(0), None, Err(())),
            (Some(5), None, Some(5), None, Err(())),
            (None, c(), None, c(), Err(())),
            (Some(5), None, None, c(), Err(())),
            (None, c(), Some(5), None, Err(())),
        ];
        for (first, after, last, before, expected) in cases {
            let got = PageRequest::from_relay(first, after.clone(), last, before.clone());
            match (&got, &expected) {
                (Ok(got), Ok(expected)) => assert_eq!(got, expected),
                (Err(MetadataError::InvalidPagination(_)), Err(())) => {}
                _ => panic!(
                    "{first:?} {after:?} {last:?} {before:?}: got {got:?}, want {expected:?}"
                ),
            }
        }
    }

    proptest! {
        /// Any accepted request pages the way its parameters name, at a size
        /// the server bounds; nothing panics.
        #[test]
        fn an_accepted_page_request_is_bounded_and_one_directional(
            first in proptest::option::of(any::<u32>()),
            after in proptest::option::of(".{0,4}"),
            last in proptest::option::of(any::<u32>()),
            before in proptest::option::of(".{0,4}"),
        ) {
            if let Ok(page) = PageRequest::from_relay(first, after.clone(), last, before.clone()) {
                prop_assert!(page.size.get() <= MAX_PAGE_SIZE);
                match page.direction {
                    PageDirection::Forward => {
                        prop_assert!(last.is_none() && before.is_none());
                        prop_assert_eq!(page.cursor, after);
                    }
                    PageDirection::Backward => {
                        prop_assert!(first.is_none() && after.is_none());
                        prop_assert_eq!(page.cursor, before);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn pages_forward_and_back_carry_their_neighbours_and_their_cursors() {
        let library = Library::new();
        for title in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
            library.movie(title).await;
        }
        let service = library.service();

        let one = service.search_media(request(Some(2), None)).await.unwrap();
        assert_eq!(titles(&one), ["Alpha", "Bravo"]);
        assert!(one.page_info.has_next_page && !one.page_info.has_previous_page);

        let two = service
            .search_media(request(Some(2), one.page_info.end_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&two), ["Charlie", "Delta"]);
        assert!(two.page_info.has_next_page && two.page_info.has_previous_page);

        let three = service
            .search_media(request(Some(2), two.page_info.end_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&three), ["Echo"]);
        assert!(!three.page_info.has_next_page && three.page_info.has_previous_page);

        let back = service
            .search_media(backward(Some(2), three.page_info.start_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(
            titles(&back),
            ["Charlie", "Delta"],
            "a backward page is in display order"
        );
        assert!(back.page_info.has_next_page && back.page_info.has_previous_page);
        let front = service
            .search_media(backward(Some(2), back.page_info.start_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&front), ["Alpha", "Bravo"]);
        assert!(front.page_info.has_next_page && !front.page_info.has_previous_page);

        let last_two = service.search_media(backward(Some(2), None)).await.unwrap();
        assert_eq!(titles(&last_two), ["Delta", "Echo"]);
        assert!(!last_two.page_info.has_next_page && last_two.page_info.has_previous_page);
    }

    #[tokio::test]
    async fn an_empty_listing_is_an_empty_page_with_no_cursors() {
        let page = Library::new()
            .service()
            .search_media(request(None, None))
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert!(!page.page_info.has_next_page && !page.page_info.has_previous_page);
        assert_eq!(page.page_info.start_cursor, None);
        assert_eq!(page.page_info.end_cursor, None);
    }

    #[tokio::test]
    async fn a_cursor_not_issued_for_this_sort_is_an_invalid_cursor() {
        let library = Library::new();
        library.movie("Alpha").await;
        library.movie("Bravo").await;
        let service = library.service();
        let page = service.search_media(request(Some(1), None)).await.unwrap();
        let cursor = page.page_info.end_cursor.expect("a cursor");

        let garbage = service
            .search_media(request(Some(1), Some("not-a-cursor".to_string())))
            .await;
        assert!(
            matches!(garbage, Err(MetadataError::InvalidCursor(_))),
            "{garbage:?}"
        );

        let resorted = service
            .search_media(BrowseRequest {
                sort_by: MediaSortField::Year,
                ..request(Some(1), Some(cursor))
            })
            .await;
        assert!(
            matches!(resorted, Err(MetadataError::InvalidCursor(_))),
            "{resorted:?}"
        );
    }

    #[tokio::test]
    async fn a_page_request_the_server_does_not_answer_is_invalid_pagination() {
        let service = Library::new().service();
        let mixed = service
            .search_media(BrowseRequest {
                last: Some(2),
                ..request(Some(2), None)
            })
            .await;
        assert!(
            matches!(mixed, Err(MetadataError::InvalidPagination(_))),
            "{mixed:?}"
        );
        let oversized = service
            .search_media(request(Some(MAX_PAGE_SIZE + 1), None))
            .await;
        assert!(
            matches!(oversized, Err(MetadataError::InvalidPagination(_))),
            "{oversized:?}"
        );
    }

    /// A NUL cannot be bound into a Postgres text parameter, so a search for
    /// one failed the statement and answered caller input with a 500. It is
    /// refused before the store is asked; any other text still searches.
    #[tokio::test]
    async fn a_search_text_holding_nul_is_refused_before_the_store() {
        for text in ["\0", "a\0b"] {
            let mut catalog = MockCatalogRepository::new();
            catalog.expect_browse().never();
            let result = Library::new()
                .service_over(Arc::new(catalog))
                .search_media(BrowseRequest {
                    filters: MediaSearchFilters {
                        query: Some(text.to_string()),
                        ..Default::default()
                    },
                    ..request(None, None)
                })
                .await;
            assert!(
                matches!(result, Err(MetadataError::InvalidSearchQuery(_))),
                "{text:?}: {result:?}"
            );
        }

        let library = Library::new();
        library.movie("Alpha").await;
        let page = library
            .service()
            .search_media(BrowseRequest {
                filters: MediaSearchFilters {
                    query: Some("alph".to_string()),
                    ..Default::default()
                },
                ..request(None, None)
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1, "{page:?}");
    }

    /// NFR-205: a database failure while browsing is an error the route turns
    /// into a 500 -- it used to become an empty page, which says the library
    /// has nothing.
    #[tokio::test]
    async fn a_failing_catalogue_is_an_internal_error_not_an_empty_page() {
        let mut catalog = MockCatalogRepository::new();
        catalog
            .expect_browse()
            .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
        let result = Library::new()
            .service_over(Arc::new(catalog))
            .search_media(request(None, None))
            .await;
        assert!(
            matches!(result, Err(MetadataError::InternalError(_))),
            "{result:?}"
        );
    }

    /// NFR-205: a failing title read is an error, never "no such title" --
    /// that is what turned a database failure on the detail route into a 404.
    #[tokio::test]
    async fn a_failing_title_read_is_an_internal_error_not_a_miss() {
        let failing = || {
            let mut movies = MockMovieRepository::new();
            movies
                .expect_find_by_id()
                .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
            movies
        };
        let service = |movies: MockMovieRepository| {
            let library = Library::new();
            DbMetadataService::new(MetadataRepositories {
                movies: Arc::new(movies),
                shows: library.shows.clone(),
                sources: super::empty_sources(),
                catalog: Arc::new(MockCatalogRepository::new()),
                genres: library.genres.clone(),
            })
        };
        let id = Uuid::new_v4();

        let detail = service(failing()).get_media_metadata(id).await;
        assert!(
            matches!(detail, Err(MetadataError::InternalError(_))),
            "{detail:?}"
        );
        let sources = service(failing()).get_media_sources(&id.to_string()).await;
        assert!(
            matches!(sources, Err(MetadataError::InternalError(_))),
            "{sources:?}"
        );
    }

    /// NFR-205: every read that hydrates a page is load-bearing. A failure
    /// in any one of them -- the movies, the shows, either kind's genres, the
    /// shows' counts -- fails the page as an internal error (a 500), never a
    /// page with those titles, genres or counts silently missing.
    #[tokio::test]
    async fn a_failing_hydration_read_fails_the_page_rather_than_thinning_it() {
        use beam_domain::repositories::genre::MockGenreRepository;
        use beam_domain::repositories::show::MockShowRepository;

        fn reset() -> DbErr {
            DbErr::Custom("connection reset".to_string())
        }

        #[derive(Debug, Clone, Copy)]
        enum Failing {
            Movies,
            Shows,
            MovieGenres,
            ShowGenres,
            ChildCounts,
        }

        for failing in [
            Failing::Movies,
            Failing::Shows,
            Failing::MovieGenres,
            Failing::ShowGenres,
            Failing::ChildCounts,
        ] {
            let library = Library::new();
            let movie = library.movie("Arrival").await;
            let show = library.full_show("Severance").await;
            let page = vec![
                CatalogPosition {
                    kind: TitleKind::Movie,
                    id: movie,
                    key: SortKey::Title("arrival".to_string()),
                },
                CatalogPosition {
                    kind: TitleKind::Show,
                    id: show,
                    key: SortKey::Title("severance".to_string()),
                },
            ];
            let mut catalog = MockCatalogRepository::new();
            catalog.expect_browse().returning(move |_| Ok(page.clone()));

            // Each read before the failing one answers from the doubles the
            // titles were written through; the failing one errs; nothing
            // after it may be asked (mockall refuses an unexpected call).
            let mut movies = MockMovieRepository::new();
            let mut shows = MockShowRepository::new();
            let mut genres = MockGenreRepository::new();
            let (movie_store, show_store, genre_store) = (
                library.movies.clone(),
                library.shows.clone(),
                library.genres.clone(),
            );
            match failing {
                Failing::Movies => {
                    movies.expect_find_by_ids().returning(|_| Err(reset()));
                }
                Failing::Shows
                | Failing::MovieGenres
                | Failing::ShowGenres
                | Failing::ChildCounts => {
                    let found = movie_store.find_by_ids(&[movie]).await.unwrap();
                    movies
                        .expect_find_by_ids()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies => {}
                Failing::Shows => {
                    shows.expect_find_by_ids().returning(|_| Err(reset()));
                }
                Failing::MovieGenres | Failing::ShowGenres | Failing::ChildCounts => {
                    let found = show_store.find_by_ids(&[show]).await.unwrap();
                    shows
                        .expect_find_by_ids()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies | Failing::Shows => {}
                Failing::MovieGenres => {
                    genres
                        .expect_movie_genre_names()
                        .returning(|_| Err(reset()));
                }
                Failing::ShowGenres | Failing::ChildCounts => {
                    let found = genre_store.movie_genre_names(&[movie]).await.unwrap();
                    genres
                        .expect_movie_genre_names()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies | Failing::Shows | Failing::MovieGenres => {}
                Failing::ShowGenres => {
                    genres.expect_show_genre_names().returning(|_| Err(reset()));
                }
                Failing::ChildCounts => {
                    let found = genre_store.show_genre_names(&[show]).await.unwrap();
                    genres
                        .expect_show_genre_names()
                        .returning(move |_| Ok(found.clone()));
                    shows.expect_child_counts().returning(|_| Err(reset()));
                }
            }

            let result = DbMetadataService::new(MetadataRepositories {
                movies: Arc::new(movies),
                shows: Arc::new(shows),
                sources: super::empty_sources(),
                catalog: Arc::new(catalog),
                genres: Arc::new(genres),
            })
            .search_media(request(None, None))
            .await;
            assert!(
                matches!(result, Err(MetadataError::InternalError(_))),
                "{failing:?} failing: {result:?}"
            );
        }
    }

    /// A title the page named but that has gone by the time it is read is
    /// left out; the page's cursors still mark where the page ended, so the
    /// next page starts after it.
    #[tokio::test]
    async fn a_title_gone_between_the_page_and_its_read_is_skipped() {
        let library = Library::new();
        let kept = library.movie("Kept").await;
        let gone = Uuid::new_v4();
        let positions = vec![
            CatalogPosition {
                kind: TitleKind::Movie,
                id: kept,
                key: SortKey::Title("kept".to_string()),
            },
            CatalogPosition {
                kind: TitleKind::Movie,
                id: gone,
                key: SortKey::Title("zz".to_string()),
            },
        ];
        let mut catalog = MockCatalogRepository::new();
        let returned = positions.clone();
        catalog
            .expect_browse()
            .returning(move |_| Ok(returned.clone()));

        let page = library
            .service_over(Arc::new(catalog))
            .search_media(request(None, None))
            .await
            .unwrap();

        assert_eq!(titles(&page), ["Kept"]);
        assert_eq!(
            page.page_info.end_cursor,
            Some(cursor::encode(
                MediaSortField::Title,
                SortOrder::Asc,
                &positions[1]
            ))
        );
    }

    /// Issue #187: a browsed show carried no genres, ratings or identifiers,
    /// and zero seasons and episodes. It carries all of them now, and agrees
    /// with its own detail.
    #[tokio::test]
    async fn a_browsed_show_carries_genres_ratings_identifiers_and_counts() {
        let library = Library::new();
        let id = library.full_show("Severance").await;
        let service = library.service();

        let page = service
            .search_media(BrowseRequest {
                filters: MediaSearchFilters {
                    media_type: Some(MediaTypeFilter::Show),
                    ..Default::default()
                },
                ..request(None, None)
            })
            .await
            .unwrap();
        let [MediaMetadata::Show(browsed)] = page.items.as_slice() else {
            panic!("one show: {page:?}");
        };
        let Some(MediaMetadata::Show(detail)) = service.get_media_metadata(id).await.unwrap()
        else {
            panic!("the show resolves");
        };

        for (view, show) in [("browse", browsed), ("detail", &detail)] {
            assert_eq!(show.genres, ["drama", "Thriller"], "{view}");
            assert_eq!(
                show.ratings.as_ref().and_then(|r| r.tmdb),
                Some(84),
                "{view}"
            );
            let identifiers = show.identifiers.as_ref().expect("identified");
            assert_eq!(identifiers.tmdb_id, Some(95396), "{view}");
            assert_eq!(identifiers.imdb_id.as_deref(), Some("tt11280740"), "{view}");
            assert_eq!((show.season_count, show.episode_count), (2, 3), "{view}");
        }
        assert!(
            browsed.seasons.is_empty(),
            "browse carries counts, not seasons"
        );
        assert_eq!(detail.seasons.len(), 2);
    }

    #[tokio::test]
    async fn a_movie_carries_its_genres_and_the_genre_filter_takes_a_name_or_a_slug() {
        let library = Library::new();
        let tagged = library.movie("Arrival").await;
        library.movie("Clue").await;
        library
            .genres
            .set_movie_genres(
                tagged,
                &["Science Fiction".to_string(), "Drama".to_string()],
            )
            .await
            .unwrap();
        let service = library.service();

        for genre in ["Science Fiction", "science fiction", "science-fiction"] {
            let page = service
                .search_media(BrowseRequest {
                    filters: MediaSearchFilters {
                        genre: Some(genre.to_string()),
                        ..Default::default()
                    },
                    ..request(None, None)
                })
                .await
                .unwrap();
            assert_eq!(titles(&page), ["Arrival"], "genre={genre}");
            let MediaMetadata::Movie(movie) = &page.items[0] else {
                panic!("a movie");
            };
            assert_eq!(movie.genres, ["Drama", "Science Fiction"]);
        }
        let Some(MediaMetadata::Movie(detail)) = service.get_media_metadata(tagged).await.unwrap()
        else {
            panic!("the movie resolves");
        };
        assert_eq!(detail.genres, ["Drama", "Science Fiction"]);
    }

    /// Every sort key reaches the store: sorting by rating orders by rating,
    /// by runtime by runtime -- not silently by title, as three of the five
    /// keys used to.
    #[tokio::test]
    async fn every_sort_key_orders_the_page_by_that_key() {
        use beam_domain::providers::enrichment::MovieEnrichment;

        let library = Library::new();
        for (title, year, runtime_mins, rating) in [
            ("Alpha", 2010, 100, 5.0),
            ("Bravo", 1990, 80, 9.0),
            ("Charlie", 2000, 120, 7.0),
        ] {
            let id = library.movie(title).await;
            library
                .movies
                .apply_enrichment(
                    id,
                    &MovieEnrichment {
                        title: title.to_string(),
                        year: Some(year),
                        runtime_mins: Some(runtime_mins),
                        rating: Some(rating),
                        ..Default::default()
                    },
                    &beam_domain::models::enrichment::FieldLocks::none(),
                )
                .await
                .unwrap();
        }
        let service = library.service();

        for (sort_by, ascending) in [
            (MediaSortField::Title, ["Alpha", "Bravo", "Charlie"]),
            (MediaSortField::Year, ["Bravo", "Charlie", "Alpha"]),
            (MediaSortField::Rating, ["Alpha", "Charlie", "Bravo"]),
            (MediaSortField::DateAdded, ["Alpha", "Bravo", "Charlie"]),
            (MediaSortField::Runtime, ["Bravo", "Alpha", "Charlie"]),
        ] {
            let asc = service
                .search_media(BrowseRequest {
                    sort_by,
                    ..request(None, None)
                })
                .await
                .unwrap();
            assert_eq!(titles(&asc), ascending, "{sort_by} asc");
            let desc = service
                .search_media(BrowseRequest {
                    sort_by,
                    sort_order: SortOrder::Desc,
                    ..request(None, None)
                })
                .await
                .unwrap();
            let mut descending = ascending;
            descending.reverse();
            assert_eq!(titles(&desc), descending, "{sort_by} desc");
        }
    }
}
