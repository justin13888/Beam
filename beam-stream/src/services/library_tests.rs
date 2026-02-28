#[cfg(test)]
mod tests {
    use crate::repositories::file::MockFileRepository;
    use crate::repositories::library::MockLibraryRepository;
    use crate::services::library::{
        InMemoryPathValidator, LibraryError, LibraryService, LocalLibraryService,
    };
    use crate::services::notification::{InMemoryNotificationService, NotificationService};
    use beam_index::models::domain::{FileStatus, Library as DomainLibrary, MediaFile};
    use beam_index::repositories::file::in_memory::InMemoryFileRepository;
    use beam_index::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_index::services::index::{IndexError, MockIndexService};
    use sea_orm::DbErr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use uuid::Uuid;

    // ── helpers ───────────────────────────────────────────────────────────────────

    fn make_service(
        mock_library_repo: MockLibraryRepository,
        mock_file_repo: MockFileRepository,
        video_dir: PathBuf,
        mock_index_service: MockIndexService,
    ) -> LocalLibraryService {
        LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(mock_file_repo),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(mock_index_service),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        )
    }

    fn make_domain_library(id: Uuid, name: &str) -> DomainLibrary {
        DomainLibrary {
            id,
            name: name.to_string(),
            root_path: PathBuf::from("/media/videos"),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        }
    }

    fn make_media_file(id: Uuid, library_id: Uuid) -> MediaFile {
        MediaFile {
            id,
            library_id,
            path: PathBuf::from("/media/videos/test.mp4"),
            hash: 0,
            size_bytes: 1024,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // ── scan_library ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_scan_library_delegates_to_index_service() {
        let mock_library_repo = MockLibraryRepository::new();
        let mock_file_repo = MockFileRepository::new();
        let video_dir = PathBuf::from("/media/videos");

        let lib_id = Uuid::new_v4().to_string();
        let lib_id_clone = lib_id.clone();
        let mut mock_index = MockIndexService::new();
        mock_index
            .expect_scan_library()
            .times(1)
            .withf(move |id| id == &lib_id_clone)
            .returning(|_| Ok(42));

        let service = make_service(mock_library_repo, mock_file_repo, video_dir, mock_index);

        let result = service.scan_library(lib_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_scan_library_propagates_index_error() {
        let mock_library_repo = MockLibraryRepository::new();
        let mock_file_repo = MockFileRepository::new();
        let video_dir = PathBuf::from("/media/videos");

        let mut mock_index = MockIndexService::new();
        mock_index
            .expect_scan_library()
            .times(1)
            .returning(|_| Err(IndexError::LibraryNotFound));

        let service = make_service(mock_library_repo, mock_file_repo, video_dir, mock_index);

        let result = service.scan_library(Uuid::new_v4().to_string()).await;
        assert!(matches!(result, Err(LibraryError::LibraryNotFound)));
    }

    #[tokio::test]
    async fn test_delete_library_returns_true() {
        let mut mock_library_repo = MockLibraryRepository::new();
        let mock_file_repo = MockFileRepository::new();
        let video_dir = PathBuf::from("/media/videos");
        let mock_index = MockIndexService::new();

        let lib_id = Uuid::new_v4();

        mock_library_repo
            .expect_find_by_id()
            .times(1)
            .returning(move |_| {
                Ok(Some(DomainLibrary {
                    id: lib_id,
                    name: "Movies".to_string(),
                    root_path: PathBuf::from("/media/movies"),
                    description: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_scan_started_at: None,
                    last_scan_finished_at: None,
                    last_scan_file_count: None,
                }))
            });

        mock_library_repo
            .expect_delete()
            .times(1)
            .returning(|_| Ok(()));

        let service = make_service(mock_library_repo, mock_file_repo, video_dir, mock_index);
        let result = service.delete_library(lib_id.to_string()).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    // ── create_library ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_library_valid_path_returns_library_stores_in_repo_publishes_notification()
    {
        let video_dir = PathBuf::from("/media/videos");
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let lib_repo_ref = Arc::clone(&lib_repo);
        let notif = Arc::new(InMemoryNotificationService::new());
        let notif_ref = Arc::clone(&notif);
        let service = LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            notif as Arc<dyn NotificationService>,
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
        );

        let result = service
            .create_library("Movies".to_string(), "/media/videos/movies".to_string())
            .await;

        assert!(result.is_ok());
        let lib = result.unwrap();
        assert_eq!(lib.name, "Movies");
        assert_eq!(lib.size, 0);

        // Library stored in repo
        let stored = lib_repo_ref.libraries.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored.values().any(|l| l.name == "Movies"));

        // Notification published
        let events = notif_ref.published_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].message.contains("Movies"));
    }

    #[tokio::test]
    async fn test_create_library_absolute_path_inside_video_dir_is_accepted() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
        );

        let result = service
            .create_library("Movies".to_string(), "/media/videos/movies".to_string())
            .await;

        assert!(
            result.is_ok(),
            "absolute path inside video_dir should be accepted: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_library_relative_path_inside_video_dir_is_accepted() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
        );

        let result = service
            .create_library("Movies".to_string(), "movies".to_string())
            .await;

        assert!(
            result.is_ok(),
            "relative path inside video_dir should be accepted: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_library_path_outside_video_dir_returns_validation_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::validation_error("path escapes root")),
        );

        let result = service
            .create_library("Outside".to_string(), "/etc/secret".to_string())
            .await;

        assert!(matches!(result, Err(LibraryError::Validation(_))));
    }

    #[tokio::test]
    async fn test_create_library_path_not_found_returns_path_not_found_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::path_not_found("no such directory")),
        );

        let result = service
            .create_library("Movies".to_string(), "/nonexistent/path".to_string())
            .await;

        assert!(matches!(result, Err(LibraryError::PathNotFound(_))));
    }

    #[tokio::test]
    async fn test_create_library_repo_db_error_returns_db_error() {
        let video_dir = PathBuf::from("/media/videos");

        let mut mock_library_repo = MockLibraryRepository::new();
        mock_library_repo
            .expect_create()
            .times(1)
            .returning(|_| Err(DbErr::Custom("insert failed".to_string())));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
        );

        let result = service
            .create_library("Movies".to_string(), "/media/videos/movies".to_string())
            .await;

        assert!(matches!(result, Err(LibraryError::Db(_))));
    }

    // ── get_libraries ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_libraries_empty_repo_returns_empty_vec() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_libraries("user1".to_string()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_get_libraries_returns_all_libraries_with_correct_file_counts() {
        let video_dir = PathBuf::from("/media/videos");
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(id1, make_domain_library(id1, "Movies"));
        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(id2, make_domain_library(id2, "Shows"));
        lib_repo.file_counts.lock().unwrap().insert(id1, 5);
        lib_repo.file_counts.lock().unwrap().insert(id2, 12);

        let service = LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_libraries("user1".to_string()).await;

        assert!(result.is_ok());
        let libs = result.unwrap();
        assert_eq!(libs.len(), 2);
        let movies = libs.iter().find(|l| l.name == "Movies").unwrap();
        assert_eq!(movies.size, 5);
        let shows = libs.iter().find(|l| l.name == "Shows").unwrap();
        assert_eq!(shows.size, 12);
    }

    #[tokio::test]
    async fn test_get_libraries_repo_find_all_db_error_returns_db_error() {
        let video_dir = PathBuf::from("/media/videos");

        let mut mock_library_repo = MockLibraryRepository::new();
        mock_library_repo
            .expect_find_all()
            .times(1)
            .returning(|| Err(DbErr::Custom("connection lost".to_string())));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_libraries("user1".to_string()).await;
        assert!(matches!(result, Err(LibraryError::Db(_))));
    }

    #[tokio::test]
    async fn test_get_libraries_count_files_db_error_propagates() {
        let video_dir = PathBuf::from("/media/videos");
        let lib_id = Uuid::new_v4();

        let mut mock_library_repo = MockLibraryRepository::new();
        mock_library_repo
            .expect_find_all()
            .times(1)
            .returning(move || Ok(vec![make_domain_library(lib_id, "Movies")]));
        mock_library_repo
            .expect_count_files()
            .times(1)
            .returning(|_| Err(DbErr::Custom("count failed".to_string())));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_libraries("user1".to_string()).await;
        assert!(matches!(result, Err(LibraryError::Db(_))));
    }

    // ── get_library_by_id ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_library_by_id_existing_library_returns_some() {
        let video_dir = PathBuf::from("/media/videos");
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let lib_id = Uuid::new_v4();

        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(lib_id, make_domain_library(lib_id, "Movies"));
        lib_repo.file_counts.lock().unwrap().insert(lib_id, 7);

        let service = LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_library_by_id(lib_id.to_string()).await;

        assert!(result.is_ok());
        let opt = result.unwrap();
        assert!(opt.is_some());
        let lib = opt.unwrap();
        assert_eq!(lib.id, lib_id.to_string());
        assert_eq!(lib.name, "Movies");
        assert_eq!(lib.size, 7);
    }

    #[tokio::test]
    async fn test_get_library_by_id_missing_library_returns_none() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_library_by_id(Uuid::new_v4().to_string()).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_library_by_id_invalid_uuid_returns_invalid_id_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service
            .get_library_by_id("not-a-valid-uuid".to_string())
            .await;
        assert!(matches!(result, Err(LibraryError::InvalidId)));
    }

    // ── get_library_files ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_library_files_existing_library_with_files_returns_all_files() {
        let video_dir = PathBuf::from("/media/videos");
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let lib_id = Uuid::new_v4();

        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(lib_id, make_domain_library(lib_id, "Movies"));

        for _ in 0..3 {
            let file_id = Uuid::new_v4();
            file_repo
                .files
                .lock()
                .unwrap()
                .insert(file_id, make_media_file(file_id, lib_id));
        }

        let service = LocalLibraryService::new(
            lib_repo,
            file_repo,
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_library_files(lib_id.to_string()).await;

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|f| f.library_id == lib_id.to_string()));
    }

    #[tokio::test]
    async fn test_get_library_files_library_not_found_returns_library_not_found_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_library_files(Uuid::new_v4().to_string()).await;

        assert!(matches!(result, Err(LibraryError::LibraryNotFound)));
    }

    #[tokio::test]
    async fn test_get_library_files_invalid_uuid_returns_invalid_id_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service
            .get_library_files("not-a-valid-uuid".to_string())
            .await;
        assert!(matches!(result, Err(LibraryError::InvalidId)));
    }

    // ── get_file_by_id ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_file_by_id_existing_file_returns_some() {
        let video_dir = PathBuf::from("/media/videos");
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let lib_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();

        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file_id, make_media_file(file_id, lib_id));

        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            file_repo,
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_file_by_id(file_id.to_string()).await;

        assert!(result.is_ok());
        let opt = result.unwrap();
        assert!(opt.is_some());
        assert_eq!(opt.unwrap().id, file_id.to_string());
    }

    #[tokio::test]
    async fn test_get_file_by_id_missing_file_returns_none() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_file_by_id(Uuid::new_v4().to_string()).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_file_by_id_invalid_uuid_returns_invalid_id_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.get_file_by_id("not-a-valid-uuid".to_string()).await;
        assert!(matches!(result, Err(LibraryError::InvalidId)));
    }

    // ── delete_library (additional cases) ────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_library_unknown_id_returns_library_not_found() {
        let video_dir = PathBuf::from("/media/videos");

        let mut mock_library_repo = MockLibraryRepository::new();
        mock_library_repo
            .expect_find_by_id()
            .times(1)
            .returning(|_| Ok(None));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.delete_library(Uuid::new_v4().to_string()).await;

        assert!(matches!(result, Err(LibraryError::LibraryNotFound)));
    }

    #[tokio::test]
    async fn test_delete_library_publishes_notification() {
        let video_dir = PathBuf::from("/media/videos");
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let lib_id = Uuid::new_v4();

        lib_repo
            .libraries
            .lock()
            .unwrap()
            .insert(lib_id, make_domain_library(lib_id, "Movies"));

        let notif = Arc::new(InMemoryNotificationService::new());
        let notif_ref = Arc::clone(&notif);

        let service = LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            notif as Arc<dyn NotificationService>,
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
        );

        let result = service.delete_library(lib_id.to_string()).await;
        assert!(result.is_ok());
        assert!(result.unwrap());

        let events = notif_ref.published_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].message.contains("Movies"));
    }
}
