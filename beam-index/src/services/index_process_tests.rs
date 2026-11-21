//! `process_new_file`: a new file indexed whole, or its failure reported.

use super::test_harness::*;
use super::*;
use crate::probe::metadata::VideoFileMetadata;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::MockFileRepository;
use beam_domain::repositories::library::MockLibraryRepository;
use beam_domain::repositories::movie::MockMovieRepository;
use beam_domain::repositories::show::MockShowRepository;
use beam_domain::repositories::stream::MockMediaStreamRepository;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn test_process_file_movie_success() {
    let mock_library_repo = MockLibraryRepository::new();
    let mut mock_file_repo = MockFileRepository::new();
    let mut mock_movie_repo = MockMovieRepository::new();
    let mock_show_repo = MockShowRepository::new();
    let mut mock_stream_repo = MockMediaStreamRepository::new();
    let mut mock_hash_service = MockHashService::new();
    let mut mock_media_info_service = MockMediaInfoService::new();

    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("movies/Avatar.mp4");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"fake movie data").unwrap();
    let lib_id = Uuid::new_v4();

    mock_media_info_service
        .expect_get_video_metadata()
        .times(1)
        .returning(|_| {
            Ok(VideoFileMetadata {
                file_path: PathBuf::from("test"),
                metadata: Default::default(),
                best_video_stream: Some(0),
                best_audio_stream: Some(1),
                best_subtitle_stream: None,
                duration: 1000000,
                streams: vec![],
                format_name: "mp4".to_string(),
                format_long_name: "MPEG-4".to_string(),
                file_size: 1024,
                bit_rate: 1000,
                probe_score: 100,
                chapters: Vec::new(),
            })
        });

    mock_hash_service
        .expect_hash_async()
        .times(1)
        .returning(|_| Ok(12345));

    let movie_id = Uuid::new_v4();
    mock_movie_repo
        .expect_find_or_create_by_identity()
        .times(1)
        .returning(move |_| {
            Ok(beam_domain::models::Movie {
                id: movie_id,
                title: "Avatar".to_string(),
                identity_key: None,
                pinned_ref: None,
                pin_source: None,
                title_localized: None,
                description: None,
                year: None,
                release_date: None,
                runtime: None,
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
            })
        });
    mock_movie_repo
        .expect_ensure_library_association()
        .times(1)
        .returning(|_, _| Ok(()));

    let entry_id = Uuid::new_v4();
    mock_movie_repo
        .expect_find_or_create_entry()
        .times(1)
        .returning(move |_| {
            Ok(beam_domain::models::MovieEntry {
                id: entry_id,
                library_id: Uuid::new_v4(),
                movie_id: Uuid::new_v4(),
                edition: None,
                created_at: chrono::Utc::now(),
            })
        });

    mock_file_repo
        .expect_find_by_hash()
        .times(1)
        .returning(|_| Ok(vec![]));
    // No row of the library has the file's content: not a moved file.
    mock_file_repo
        .expect_find_by_library_and_hash_including_missing()
        .times(1)
        .returning(|_, _| Ok(vec![]));

    let file_id = Uuid::new_v4();
    mock_file_repo.expect_create().times(1).returning(move |_| {
        Ok(beam_domain::models::MediaFile {
            id: file_id,
            library_id: Uuid::new_v4(),
            path: PathBuf::from("test"),
            hash: 12345,
            size_bytes: 1024,
            mtime: None,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: Some(beam_domain::models::MediaFileContent::movie(entry_id)),
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        })
    });

    mock_stream_repo
        .expect_insert_streams()
        .times(1)
        .returning(|_| Ok(0u32));

    let service = LocalIndexService::new(
        Arc::new(mock_library_repo),
        Arc::new(mock_file_repo),
        Arc::new(mock_movie_repo),
        Arc::new(mock_show_repo),
        Arc::new(mock_stream_repo),
        Arc::new(mock_hash_service),
        Arc::new(mock_media_info_service),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let result = service
        .process_new_file(
            &path,
            &test_library(lib_id, temp_dir.path()),
            RelinkSource::Repository,
            None,
            Inodes::Stable,
        )
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), FileOutcome::Added);
}

#[tokio::test]
async fn test_process_file_episode_success() {
    let mock_library_repo = MockLibraryRepository::new();
    let mut mock_file_repo = MockFileRepository::new();
    let mock_movie_repo = MockMovieRepository::new();
    let mut mock_show_repo = MockShowRepository::new();
    let mut mock_stream_repo = MockMediaStreamRepository::new();
    let mut mock_hash_service = MockHashService::new();
    let mut mock_media_info_service = MockMediaInfoService::new();

    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir
        .path()
        .join("shows/The Show/Season 1/The Show - S01E01.mkv");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"fake episode data").unwrap();
    let lib_id = Uuid::new_v4();

    mock_media_info_service
        .expect_get_video_metadata()
        .times(1)
        .returning(|_| {
            Ok(VideoFileMetadata {
                file_path: PathBuf::from("test"),
                metadata: Default::default(),
                best_video_stream: Some(0),
                best_audio_stream: Some(1),
                best_subtitle_stream: None,
                duration: 1800000000,
                streams: vec![],
                format_name: "mkv".to_string(),
                format_long_name: "Matroska".to_string(),
                file_size: 500 * 1024 * 1024,
                bit_rate: 2000,
                probe_score: 100,
                chapters: Vec::new(),
            })
        });

    mock_hash_service
        .expect_hash_async()
        .times(1)
        .returning(|_| Ok(67890));

    let show_id = Uuid::new_v4();
    mock_show_repo
        .expect_find_or_create_by_identity()
        .times(1)
        .returning(move |_| {
            Ok(beam_domain::models::Show {
                id: show_id,
                title: "Season 1".to_string(),
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
            })
        });
    mock_show_repo
        .expect_ensure_library_association()
        .times(1)
        .returning(|_, _| Ok(()));

    let season_id = Uuid::new_v4();
    mock_show_repo
        .expect_find_or_create_season()
        .times(1)
        .returning(move |_, _| {
            Ok(beam_domain::models::Season {
                id: season_id,
                show_id,
                season_number: 1,
                poster_url: None,
                first_aired: None,
                last_aired: None,
            })
        });

    let episode_id = Uuid::new_v4();
    mock_show_repo
        .expect_find_or_create_episode()
        .times(1)
        .returning(move |_| {
            Ok(beam_domain::models::Episode {
                id: episode_id,
                season_id,
                episode_number: 1,
                title: "The Show - S01E01".to_string(),
                description: None,
                air_date: None,
                runtime: None,
                thumbnail_url: None,
                created_at: chrono::Utc::now(),
            })
        });

    mock_file_repo
        .expect_find_by_hash()
        .times(1)
        .returning(|_| Ok(vec![]));
    // No row of the library has the file's content: not a moved file.
    mock_file_repo
        .expect_find_by_library_and_hash_including_missing()
        .times(1)
        .returning(|_, _| Ok(vec![]));

    let file_id = Uuid::new_v4();
    mock_file_repo.expect_create().times(1).returning(move |_| {
        Ok(beam_domain::models::MediaFile {
            id: file_id,
            library_id: Uuid::new_v4(),
            path: PathBuf::from("test"),
            hash: 67890,
            size_bytes: 500 * 1024 * 1024,
            mtime: None,
            identity: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: None,
            container_format: None,
            content: Some(beam_domain::models::MediaFileContent::episode(episode_id)),
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        })
    });

    mock_stream_repo
        .expect_insert_streams()
        .times(1)
        .returning(|_| Ok(0u32));

    let service = LocalIndexService::new(
        Arc::new(mock_library_repo),
        Arc::new(mock_file_repo),
        Arc::new(mock_movie_repo),
        Arc::new(mock_show_repo),
        Arc::new(mock_stream_repo),
        Arc::new(mock_hash_service),
        Arc::new(mock_media_info_service),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let result = service
        .process_new_file(
            &path,
            &test_library(lib_id, temp_dir.path()),
            RelinkSource::Repository,
            None,
            Inodes::Stable,
        )
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), FileOutcome::Added);
}

#[tokio::test]
async fn test_process_file_missing_path_reports_no_filesystem_path() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("movies/Vanished (2020).mkv");
    // Nothing is created: metadata cannot be read, so the file is refused
    // before any repository, hash, or probe call.
    let service = LocalIndexService::new(
        Arc::new(MockLibraryRepository::new()),
        Arc::new(MockFileRepository::new()),
        Arc::new(MockMovieRepository::new()),
        Arc::new(MockShowRepository::new()),
        Arc::new(MockMediaStreamRepository::new()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let err = service
        .process_new_file(
            &path,
            &test_library(Uuid::new_v4(), temp_dir.path()),
            RelinkSource::Repository,
            None,
            Inodes::Stable,
        )
        .await
        .expect_err("a file that cannot be stat'ed must be refused");
    assert!(matches!(err, IndexError::PathNotFound(_)));
    assert_names_no_path(&err, &[&path]);
}

#[tokio::test]
async fn test_process_file_hash_failure_reports_no_filesystem_path() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("Vanished (2020).mkv");
    std::fs::write(&path, b"video data").unwrap();

    // The error the real hasher yields when the file goes away between the
    // walk and the hash: an OS error from opening that very path. Whether
    // its `Display` carries the path is precisely what the guarantee on
    // `IndexError::PathNotFound` rests on at this construction site, so the
    // error has to be a real one rather than a hand-written string.
    let io_error = std::fs::File::open(temp_dir.path().join("Vanished (2020).mkv.part"))
        .expect_err("that file was never created");

    // No prober expectation: a file is hashed before it is probed, so a
    // hash failure is reported without a probe.
    let mock_media_info = MockMediaInfoService::new();

    let mut mock_hash = MockHashService::new();
    mock_hash
        .expect_hash_async()
        .times(1)
        .return_once(move |_| Err(io_error));

    let service = LocalIndexService::new(
        Arc::new(MockLibraryRepository::new()),
        Arc::new(MockFileRepository::new()),
        Arc::new(MockMovieRepository::new()),
        Arc::new(MockShowRepository::new()),
        Arc::new(MockMediaStreamRepository::new()),
        Arc::new(mock_hash),
        Arc::new(mock_media_info),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let err = service
        .process_new_file(
            &path,
            &test_library(Uuid::new_v4(), temp_dir.path()),
            RelinkSource::Repository,
            None,
            Inodes::Stable,
        )
        .await
        .expect_err("a file that cannot be hashed must be refused");
    assert!(matches!(err, IndexError::PathNotFound(_)));
    assert_names_no_path(&err, &[&path]);
}
