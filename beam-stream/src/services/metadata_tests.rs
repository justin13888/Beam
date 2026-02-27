/// Tests for DbMetadataService using in-memory repository fakes.
///
/// These tests exercise the full metadata service vertical slice without any
/// external infrastructure. All repositories are stateful in-memory fakes.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use crate::models::domain::movie::Movie;
    use crate::models::domain::{Episode, MediaFile, MediaFileContent, MovieEntry, Season, Show};
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use crate::services::metadata::{
        DbMetadataService, MediaFilter, MediaSearchFilters, MediaSortField, MetadataService,
        SortOrder,
    };

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
            mime_type: Some("video/mp4".to_string()),
            duration: Some(Duration::from_secs(7200)),
            container_format: Some("mp4".to_string()),
            content: Some(content),
            status: crate::models::domain::FileStatus::Known,
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
                assert_eq!(m.title.original, "Test Movie");
                assert_eq!(m.year, Some(2023));
                assert!(m.duration.is_some());
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
                assert_eq!(s.title.original, "Test Show");
                assert_eq!(s.year, Some(2022));
                assert_eq!(s.seasons.len(), 1);
                assert_eq!(s.seasons[0].episodes.len(), 1);
                assert_eq!(s.seasons[0].episodes[0].title, "Pilot");
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
}
