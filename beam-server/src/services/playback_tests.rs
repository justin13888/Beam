/// Tests for DbPlaybackService using in-memory repository fakes.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use crate::services::playback::{DbPlaybackService, PlaybackError, PlaybackService};
    use beam_domain::models::movie::Movie;
    use beam_domain::models::show::Show;
    use beam_domain::models::{Episode, MediaFile, MediaFileContent, MovieEntry, Season};
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;

    fn make_movie(title: &str) -> Movie {
        Movie {
            id: Uuid::new_v4(),
            title: title.to_string(),
            title_localized: None,
            description: None,
            year: None,
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

    fn make_media_file(content: MediaFileContent) -> MediaFile {
        use std::path::PathBuf;
        MediaFile {
            id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
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

    struct Harness {
        service: DbPlaybackService,
        file_repo: Arc<InMemoryFileRepository>,
        movie_repo: Arc<InMemoryMovieRepository>,
        show_repo: Arc<InMemoryShowRepository>,
    }

    fn make_harness() -> Harness {
        let playback_repo = Arc::new(InMemoryPlaybackProgressRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());

        let service = DbPlaybackService::new(
            playback_repo,
            file_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
        );

        Harness {
            service,
            file_repo,
            movie_repo,
            show_repo,
        }
    }

    #[tokio::test]
    async fn report_progress_unknown_file_returns_not_found() {
        let harness = make_harness();
        let result = harness
            .service
            .report_progress(Uuid::new_v4(), Uuid::new_v4(), 10.0, Some(100.0))
            .await;
        assert!(matches!(result, Err(PlaybackError::FileNotFound)));
    }

    #[tokio::test]
    async fn report_progress_known_file_upserts_and_returns_dto() {
        let harness = make_harness();
        let file = make_media_file(MediaFileContent::Movie {
            movie_entry_id: Uuid::new_v4(),
        });
        let file_id = file.id;
        harness
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let user_id = Uuid::new_v4();
        let dto = harness
            .service
            .report_progress(user_id, file_id, 42.0, Some(100.0))
            .await
            .unwrap();

        assert_eq!(dto.file_id, file_id.to_string());
        assert_eq!(dto.position_secs, 42.0);
        assert!(!dto.completed);
    }

    #[tokio::test]
    async fn get_continue_watching_resolves_movie_via_entry() {
        let harness = make_harness();

        let movie = make_movie("Test Movie");
        let movie_id = movie.id;
        harness
            .movie_repo
            .movies
            .lock()
            .unwrap()
            .insert(movie.id, movie);

        let entry = MovieEntry {
            id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            movie_id,
            edition: None,
            is_primary: true,
            created_at: chrono::Utc::now(),
        };
        let entry_id = entry.id;
        harness
            .movie_repo
            .entries
            .lock()
            .unwrap()
            .insert(entry.id, entry);

        let file = make_media_file(MediaFileContent::Movie {
            movie_entry_id: entry_id,
        });
        let file_id = file.id;
        harness
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let user_id = Uuid::new_v4();
        harness
            .service
            .report_progress(user_id, file_id, 10.0, Some(100.0))
            .await
            .unwrap();

        let items = harness
            .service
            .get_continue_watching(user_id, 10)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_id, movie_id.to_string());
        assert_eq!(items[0].media_type, "movie");
        assert_eq!(items[0].episode_id, None);
    }

    #[tokio::test]
    async fn get_continue_watching_resolves_episode_via_season_and_show() {
        let harness = make_harness();

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
        harness
            .show_repo
            .shows
            .lock()
            .unwrap()
            .insert(show.id, show);

        let season = Season {
            id: Uuid::new_v4(),
            show_id,
            season_number: 1,
            poster_url: None,
            first_aired: None,
            last_aired: None,
        };
        let season_id = season.id;
        harness
            .show_repo
            .seasons
            .lock()
            .unwrap()
            .insert(season.id, season);

        let episode = Episode {
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
        let episode_id = episode.id;
        harness
            .show_repo
            .episodes
            .lock()
            .unwrap()
            .insert(episode.id, episode);

        let file = make_media_file(MediaFileContent::Episode { episode_id });
        let file_id = file.id;
        harness
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let user_id = Uuid::new_v4();
        harness
            .service
            .report_progress(user_id, file_id, 10.0, Some(100.0))
            .await
            .unwrap();

        let items = harness
            .service
            .get_continue_watching(user_id, 10)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_id, show_id.to_string());
        assert_eq!(items[0].media_type, "show");
        assert_eq!(items[0].episode_id, Some(episode_id.to_string()));
    }

    #[tokio::test]
    async fn get_continue_watching_skips_rows_whose_file_no_longer_exists() {
        let harness = make_harness();
        let file = make_media_file(MediaFileContent::Movie {
            movie_entry_id: Uuid::new_v4(),
        });
        let file_id = file.id;
        harness
            .file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, file);

        let user_id = Uuid::new_v4();
        harness
            .service
            .report_progress(user_id, file_id, 10.0, Some(100.0))
            .await
            .unwrap();

        // Simulate the file being removed by a rescan after progress was recorded.
        harness.file_repo.files.lock().unwrap().remove(&file_id);

        let items = harness
            .service
            .get_continue_watching(user_id, 10)
            .await
            .unwrap();
        assert!(items.is_empty());
    }
}
