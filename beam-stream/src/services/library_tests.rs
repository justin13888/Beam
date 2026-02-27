#[cfg(test)]
mod tests {
    use crate::repositories::file::MockFileRepository;
    use crate::repositories::library::MockLibraryRepository;
    use crate::services::library::{InMemoryPathValidator, LocalLibraryService};
    use crate::services::notification::InMemoryNotificationService;
    use beam_index::models::domain::Library as DomainLibrary;
    use beam_index::services::index::MockIndexService;
    use std::path::PathBuf;
    use std::sync::Arc;
    use uuid::Uuid;

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

    #[tokio::test]
    async fn test_scan_library_delegates_to_index_service() {
        use crate::services::library::LibraryService;

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
        use crate::services::library::{LibraryError, LibraryService};
        use beam_index::services::index::IndexError;

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
        use crate::services::library::LibraryService;

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
}
