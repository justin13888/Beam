//! Tests for DbMetadataService using in-memory repository fakes.
//!
//! These tests exercise the full metadata service vertical slice without any
//! external infrastructure. All repositories are stateful in-memory fakes.
//!
//! The source-ranking tests are in `sources_tests.rs` and the browse tests in
//! `metadata_browse_tests.rs`; the builders below are shared with the former.

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use crate::models::search::{MediaSortField, SortOrder};
    use crate::services::metadata::{
        BrowseRequest, DbMetadataService, MediaSearchFilters, MetadataRepositories, MetadataService,
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

    /// A source's stream and download URLs both name `file_id`, and are two
    /// URLs. How they are spelled is not restated here: that each is a served
    /// route delivering the file it names is `access_tests`'
    /// `every_url_a_source_carries_is_served_and_delivers_what_it_names`.
    pub(crate) fn assert_urls_name(stream_url: &str, download_url: &str, file_id: Uuid) {
        let id = file_id.to_string();
        assert!(stream_url.contains(&id), "{stream_url} names {id}");
        assert!(download_url.contains(&id), "{download_url} names {id}");
        assert_ne!(stream_url, download_url);
    }

    pub(crate) fn make_movie(title: &str, year: Option<u32>) -> Movie {
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
            content_rating: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    pub(crate) fn make_media_file(library_id: Uuid, content: MediaFileContent) -> MediaFile {
        MediaFile {
            size_bytes: 1024,
            duration: Some(Duration::from_secs(7200)),
            content: Some(content),
            status: beam_domain::models::FileStatus::Known,
            ..beam_domain::test_utils::media_file(library_id, "/media/test.mp4")
        }
    }

    /// The real service over the given doubles, with a catalogue and a genre
    /// store reading the same title doubles.
    pub(crate) fn service(
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

    pub(crate) fn service_with_sidecars(
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
            sources: Arc::new(SourceCatalog::new(crate::services::sources::SourceRepositories {movies: movies.clone(), files, streams, sidecars, chapters: Arc::new(beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository::default()), trickplay: Arc::new(beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository::default()),})),
            movies,
            shows,
            genres,
            people: Arc::new(
                beam_domain::repositories::person::in_memory::InMemoryPersonRepository::default(),
            ),
            collections: Arc::new(beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default()),
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

        let file = make_media_file(library_id, MediaFileContent::movie(entry_id));
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
                assert!(m.duration_secs.is_some());
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
            content_rating: None,
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
        let result = service.get_media_sources(Uuid::new_v4()).await;
        assert!(matches!(result, Err(MetadataError::MediaNotFound)));
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
            content_rating: None,
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

        let result = service.get_media_sources(show_id).await;
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

        let file = make_media_file(library_id, MediaFileContent::movie(entry_id));
        let file_id = file.id;
        file_repo.files.lock().unwrap().insert(file.id, file);

        let service = service(
            movie_repo,
            Arc::new(InMemoryShowRepository::default()),
            file_repo,
            stream_repo,
        );

        let sources = service.get_media_sources(movie_id).await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].file_id, file_id);
        assert!(sources[0].is_primary);
        assert_urls_name(&sources[0].stream_url, &sources[0].download_url, file_id);
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
        let content = MediaFileContent::movie(entry.id);
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

        let sources = service.get_media_sources(movie_id).await.unwrap();
        let ids: Vec<Uuid> = sources.iter().map(|s| s.file_id).collect();
        assert_eq!(ids, vec![present_id]);
    }

    /// Seeds a show/season/episode and returns the episode id, so the sources
    /// tests can drive the episode branch of `get_media_sources`.
    pub(crate) fn seed_episode(show_repo: &InMemoryShowRepository) -> Uuid {
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
            content_rating: None,
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

        let sources = service.get_media_sources(episode_id).await.unwrap();
        assert_eq!(sources.len(), 2);

        // Two files alike in every other way are ranked by id, whatever
        // order the store returned them in.
        let mut expected = [file_a_id, file_b_id];
        expected.sort();

        for (source, id) in sources.iter().zip(expected.iter()) {
            assert_eq!(source.file_id, *id);
            assert_urls_name(&source.stream_url, &source.download_url, *id);
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
        let sources = service.get_media_sources(episode_id).await.unwrap();
        assert!(sources.is_empty());
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
            content_rating: None,
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
        use crate::models::search::TitleKind;

        MediaSearchFilters {
            media_type: Some(TitleKind::Show),
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
                access: beam_domain::models::access::AccessPolicy::unrestricted(),
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
