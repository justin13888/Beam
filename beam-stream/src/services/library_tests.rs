#[cfg(test)]
mod tests {
    // use super::*;
    use crate::models::domain::file::{FileStatus, MediaFile, MediaFileContent};
    use crate::repositories::file::MockFileRepository;
    use crate::repositories::library::MockLibraryRepository;
    use crate::repositories::movie::MockMovieRepository;
    use crate::repositories::show::MockShowRepository;
    use crate::repositories::stream::MockMediaStreamRepository;
    use crate::services::admin_log::NoOpAdminLogService;
    use crate::services::hash::MockHashService;
    use crate::services::library::LocalLibraryService;
    use crate::services::media_info::MockMediaInfoService;
    use crate::utils::metadata::VideoFileMetadata;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_process_file_movie_success() {
        // Setup Mocks
        let mut mock_library_repo = MockLibraryRepository::new();
        let mut mock_file_repo = MockFileRepository::new();
        let mut mock_movie_repo = MockMovieRepository::new();
        let mut mock_show_repo = MockShowRepository::new();
        let mut mock_stream_repo = MockMediaStreamRepository::new();
        let mut mock_hash_service = MockHashService::new();
        let mut mock_media_info_service = MockMediaInfoService::new();

        // Config
        let temp_dir = TempDir::new().unwrap().keep();
        // Config
        let temp_dir = TempDir::new().unwrap().into_path();
        let video_dir = temp_dir.clone();

        // File Path
        let path = temp_dir.join("movies/Avatar.mp4");
        let lib_id = Uuid::new_v4();

        // Mocks Expectations

        // File Repo: Check if exists - REMOVED from process_new_file
        // mock_file_repo.expect_find_by_path()...

        // Media Info: Get Metadata
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
                })
            });

        // Hash Service
        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(12345));

        // Movie Repo: Find by title
        mock_movie_repo
            .expect_find_by_title()
            .times(1)
            .returning(|_| Ok(None)); // Not found

        // Movie Repo: Create
        let movie_id = Uuid::new_v4();
        mock_movie_repo
            .expect_create()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::Movie {
                    id: movie_id,
                    title: "Avatar".to_string(),
                    runtime: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                })
            });

        // Movie Repo: Ensure Library Association
        mock_movie_repo
            .expect_ensure_library_association()
            .times(1)
            .returning(|_, _| Ok(()));

        // Movie Repo: Create Entry
        let entry_id = Uuid::new_v4();
        mock_movie_repo
            .expect_create_entry()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::MovieEntry {
                    id: entry_id,
                    library_id: Uuid::new_v4(),
                    movie_id: Uuid::new_v4(),
                    edition: None,
                    is_primary: true,
                    created_at: chrono::Utc::now(),
                })
            });

        // File Repo: Create
        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 12345,
                size_bytes: 1024,
                mime_type: Some("video/mp4".to_string()),
                duration: None,
                container_format: None,
                content: Some(MediaFileContent::Movie {
                    movie_entry_id: entry_id,
                }),
                status: FileStatus::Known,
                scanned_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });

        // Stream Repo: Insert streams
        mock_stream_repo
            .expect_insert_streams()
            .times(1)
            .returning(|_| Ok(0u32));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(mock_file_repo),
            Arc::new(mock_movie_repo),
            Arc::new(mock_show_repo),
            Arc::new(mock_stream_repo),
            video_dir,
            Arc::new(mock_hash_service),
            Arc::new(mock_media_info_service),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.process_new_file(&path, lib_id).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_process_file_episode_success() {
        // Setup Mocks
        let mut mock_library_repo = MockLibraryRepository::new();
        let mut mock_file_repo = MockFileRepository::new();
        let mut mock_movie_repo = MockMovieRepository::new();
        let mut mock_show_repo = MockShowRepository::new();
        let mut mock_stream_repo = MockMediaStreamRepository::new();
        let mut mock_hash_service = MockHashService::new();
        let mut mock_media_info_service = MockMediaInfoService::new();

        // Config
        let temp_dir = TempDir::new().unwrap();
        let video_dir = temp_dir.path().to_path_buf();

        // File Path
        let path = temp_dir
            .path()
            .join("shows/The Show/Season 1/The Show - S01E01.mkv");
        let lib_id = Uuid::new_v4();

        // Mocks Expectations

        // File Repo: Check if exists
        // File Repo: Check if exists - REMOVED

        // Media Info: Get Metadata
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
                    duration: 1800000000, // 30 mins
                    streams: vec![],
                    format_name: "mkv".to_string(),
                    format_long_name: "Matroska".to_string(),
                    file_size: 500 * 1024 * 1024,
                    bit_rate: 2000,
                    probe_score: 100,
                })
            });

        // Hash Service
        mock_hash_service
            .expect_hash_async()
            .times(1)
            .returning(|_| Ok(67890));

        // Show Repo: Find by title
        mock_show_repo
            .expect_find_by_title()
            .times(1)
            .returning(|_| Ok(None)); // Not found

        // Show Repo: Create
        let show_id = Uuid::new_v4();
        mock_show_repo.expect_create().times(1).returning(move |_| {
            Ok(crate::models::domain::Show {
                id: show_id,
                title: "Season 1".to_string(), // Guessed from parent dir logic in current impl?
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });

        // Show Repo: Ensure Library Association
        mock_show_repo
            .expect_ensure_library_association()
            .times(1)
            .returning(|_, _| Ok(()));

        // Show Repo: Find or Create Season
        let season_id = Uuid::new_v4();
        mock_show_repo
            .expect_find_or_create_season()
            .times(1)
            .returning(move |_, _| {
                Ok(crate::models::domain::Season {
                    id: season_id,
                    show_id,
                    season_number: 1,
                })
            });

        // Show Repo: Create Episode
        let episode_id = Uuid::new_v4();
        mock_show_repo
            .expect_create_episode()
            .times(1)
            .returning(move |_| {
                Ok(crate::models::domain::Episode {
                    id: episode_id,
                    season_id,
                    episode_number: 1,
                    title: "The Show - S01E01".to_string(), // file_stem
                    runtime: None,
                    created_at: chrono::Utc::now(),
                })
            });

        // File Repo: Create
        let file_id = Uuid::new_v4();
        mock_file_repo.expect_create().times(1).returning(move |_| {
            Ok(MediaFile {
                id: file_id,
                library_id: Uuid::new_v4(),
                path: PathBuf::from("test"),
                hash: 67890,
                size_bytes: 500 * 1024 * 1024,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: None,
                content: Some(MediaFileContent::Episode { episode_id }),
                status: FileStatus::Known,
                scanned_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
        });

        // Stream Repo: Insert streams
        mock_stream_repo
            .expect_insert_streams()
            .times(1)
            .returning(|_| Ok(0u32));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(mock_file_repo),
            Arc::new(mock_movie_repo),
            Arc::new(mock_show_repo),
            Arc::new(mock_stream_repo),
            video_dir,
            Arc::new(mock_hash_service),
            Arc::new(mock_media_info_service),
            Arc::new(NoOpAdminLogService),
        );

        let result = service.process_new_file(&path, lib_id).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }
}
