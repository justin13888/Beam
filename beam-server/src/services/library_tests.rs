#[cfg(test)]
mod tests {
    use crate::services::library::{
        InMemoryPathValidator, LibraryError, LibraryService, LocalLibraryService, LocatedFile,
    };
    use crate::services::notification::{InMemoryNotificationService, NotificationService};
    use beam_domain::models::{FileStatus, Library as DomainLibrary, MediaFile};
    use beam_domain::repositories::file::MockFileRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::library::MockLibraryRepository;
    use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
    use beam_index::services::index::{IndexError, MockIndexService};
    use beam_index::services::scan::{ScanCoordinator, ScanTrigger, StoppedScan};
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(mock_index_service),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        )
    }

    /// What a double's `stop_scan` answers: a real retirement, of a
    /// coordinator no one else reads.
    fn stopped_scan(library_id: Uuid, in_time: bool) -> StoppedScan {
        StoppedScan {
            in_time,
            retirement: ScanCoordinator::new().retire(library_id),
        }
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
            mtime: None,
            identity: None,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: Some("mp4".to_string()),
            content: None,
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
        }
    }

    // ── start_scan ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn start_scan_reports_a_running_scan_as_scan_in_progress() {
        let lib_id = Uuid::new_v4();
        let mut mock_index = MockIndexService::new();
        mock_index
            .expect_begin_scan()
            .times(1)
            .withf(move |id, trigger| *id == lib_id && *trigger == ScanTrigger::Manual)
            .returning(|_, _| Err(IndexError::ScanInProgress));
        mock_index.expect_run_scan().never();
        let service = make_service(
            MockLibraryRepository::new(),
            MockFileRepository::new(),
            PathBuf::from("/media/videos"),
            mock_index,
        );

        let result = service.start_scan(lib_id).await;
        assert!(matches!(result, Err(LibraryError::ScanInProgress)));
    }

    /// Deleting a library stops its scan -- cancelled and waited for --
    /// before the rows go, so the scan never writes for a library that is
    /// gone, and forgets the library's scan slot after. The order is the
    /// contract, so the calls are sequenced across both doubles.
    #[tokio::test]
    async fn deleting_a_library_stops_its_scan_first_and_forgets_it_after() {
        let mut sequence = mockall::Sequence::new();
        let mut mock_library_repo = MockLibraryRepository::new();
        let mock_file_repo = MockFileRepository::new();
        let video_dir = PathBuf::from("/media/videos");
        let lib_id = Uuid::new_v4();
        let mut mock_index = MockIndexService::new();

        mock_library_repo
            .expect_find_by_id()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| Ok(Some(make_domain_library(lib_id, "Movies"))));
        mock_index
            .expect_stop_scan()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(move |id| *id == lib_id)
            .returning(|id| stopped_scan(id, true));
        mock_library_repo
            .expect_delete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(move |id| *id == lib_id)
            .returning(|_| Ok(()));
        mock_index
            .expect_forget_library()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(move |id| *id == lib_id)
            .return_const(());

        let service = make_service(mock_library_repo, mock_file_repo, video_dir, mock_index);
        let result = service.delete_library(lib_id.to_string()).await;
        assert!(matches!(result, Ok(true)), "{result:?}");
    }

    /// A scan that does not stop in time does not hold the delete up.
    #[tokio::test]
    async fn a_scan_that_does_not_stop_in_time_does_not_block_the_delete() {
        let mut mock_library_repo = MockLibraryRepository::new();
        let lib_id = Uuid::new_v4();
        let mut mock_index = MockIndexService::new();
        mock_library_repo
            .expect_find_by_id()
            .returning(move |_| Ok(Some(make_domain_library(lib_id, "Movies"))));
        mock_index
            .expect_stop_scan()
            .returning(|id| stopped_scan(id, false));
        mock_library_repo
            .expect_delete()
            .times(1)
            .returning(|_| Ok(()));
        mock_index.expect_forget_library().times(1).return_const(());

        let service = make_service(
            mock_library_repo,
            MockFileRepository::new(),
            PathBuf::from("/media/videos"),
            mock_index,
        );
        let result = service.delete_library(lib_id.to_string()).await;
        assert!(matches!(result, Ok(true)), "{result:?}");
    }

    /// A delete that fails leaves the library stored and listed, so it must
    /// not stay retired: afterwards a watcher event for it is reconciled and
    /// a scan of it registers, as before the delete was asked for (NFR-205).
    #[tokio::test]
    async fn a_library_whose_delete_fails_is_reconciled_and_scanned_again() {
        use beam_domain::repositories::FileRepository;
        use beam_index::services::media_info::MockMediaInfoService;
        use beam_index::services::watcher::FsEventKind;
        use beam_index::services::{
            IndexService, LocalHashService, LocalIndexService, NoOpAdminLogService,
            ReconcileOutcome,
        };

        let root = tempfile::TempDir::new().unwrap();
        let film = root.path().join("Heat (1995).mkv");
        std::fs::write(&film, b"heat").unwrap();
        let lib_id = Uuid::new_v4();
        let library = DomainLibrary {
            root_path: root.path().to_path_buf(),
            ..make_domain_library(lib_id, "Movies")
        };

        let index_libraries = Arc::new(InMemoryLibraryRepository::default());
        index_libraries
            .libraries
            .lock()
            .unwrap()
            .insert(lib_id, library.clone());
        let files = Arc::new(InMemoryFileRepository::default());
        let mut prober = MockMediaInfoService::new();
        prober.expect_get_video_metadata().returning(|_| {
            Err(beam_index::probe::metadata::MetadataError::UnknownError(
                "not a film".to_string(),
            ))
        });
        let index = Arc::new(LocalIndexService::new(
            index_libraries,
            files.clone(),
            Arc::new(beam_domain::repositories::movie::in_memory::InMemoryMovieRepository::default()),
            Arc::new(beam_domain::repositories::show::in_memory::InMemoryShowRepository::default()),
            Arc::new(
                beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository::default(),
            ),
            Arc::new(LocalHashService::default()),
            Arc::new(prober),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        ));

        let mut libraries = MockLibraryRepository::new();
        libraries
            .expect_find_by_id()
            .returning(move |_| Ok(Some(library.clone())));
        libraries
            .expect_delete()
            .times(1)
            .returning(|_| Err(DbErr::Custom("statement timeout".to_string())));
        let service = LocalLibraryService::new(
            Arc::new(libraries),
            Arc::new(MockFileRepository::new()),
            root.path().to_path_buf(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            index.clone() as Arc<dyn IndexService>,
            Arc::new(InMemoryPathValidator::success(root.path().to_path_buf())),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service.delete_library(lib_id.to_string()).await;
        assert!(matches!(result, Err(LibraryError::Db(_))), "{result:?}");

        assert_eq!(
            index
                .reconcile_path(lib_id, film.clone(), FsEventKind::Created)
                .await
                .unwrap(),
            ReconcileOutcome::Done
        );
        assert!(
            files
                .find_by_path(&film.to_string_lossy())
                .await
                .unwrap()
                .is_some(),
            "the watcher event indexed the file"
        );
        let ticket = index
            .begin_scan(lib_id, ScanTrigger::Manual)
            .await
            .expect("a scan of the library registers");
        index.run_scan(ticket).await.unwrap();
    }

    // ── create_library ────────────────────────────────────────────────────────────

    /// A watcher whose registrations wait for the test to let each one
    /// through, as a registration walking a large network share would.
    #[derive(Debug)]
    struct GatedWatcher {
        inner: beam_index::services::watcher::InMemoryFsWatcher,
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    #[async_trait::async_trait]
    impl beam_index::services::watcher::FsWatcher for GatedWatcher {
        fn watch_library(
            &self,
            library_id: Uuid,
            root: &std::path::Path,
        ) -> Result<
            beam_index::services::watch_status::WatchMode,
            beam_index::services::watcher::WatchError,
        > {
            self.gate
                .lock()
                .unwrap()
                .recv()
                .expect("the test lets the registration through");
            self.inner.watch_library(library_id, root)
        }

        fn unwatch_library(
            &self,
            library_id: Uuid,
        ) -> Result<(), beam_index::services::watcher::WatchError> {
            self.inner.unwatch_library(library_id)
        }

        fn poll_once(&self) -> Vec<Uuid> {
            self.inner.poll_once()
        }

        fn registered_libraries(&self) -> Vec<Uuid> {
            self.inner.registered_libraries()
        }

        async fn next_event(&self) -> Option<beam_index::services::watcher::FsEvent> {
            self.inner.next_event().await
        }
    }

    /// Registering a new library's watch can walk its whole tree, so the
    /// create request does not wait for it: it answers while the
    /// registration is still held, and the registration lands afterwards.
    #[tokio::test]
    async fn creating_a_library_answers_before_its_watch_is_registered() {
        let video_dir = PathBuf::from("/media/videos");
        let (open_gate, gate) = std::sync::mpsc::channel();
        let watcher = Arc::new(GatedWatcher {
            inner: beam_index::services::watcher::InMemoryFsWatcher::new(),
            gate: std::sync::Mutex::new(gate),
        });
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
            Arc::new(beam_index::runtime::LibraryWatches::new(Some(
                watcher.clone(),
            ))),
        );

        let created = service
            .create_library("Movies".to_string(), "movies".to_string())
            .await
            .expect("the library is created while its registration is held");
        let id = Uuid::parse_str(&created.id).unwrap();
        assert!(
            watcher.inner.watched_libraries().is_empty(),
            "not registered yet"
        );

        open_gate.send(()).unwrap();
        watcher.inner.until_watched(id).await;
        assert_eq!(watcher.inner.watched_libraries(), vec![id]);
    }

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
            PathBuf::from("/beam-data"),
            notif as Arc<dyn NotificationService>,
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
    async fn test_create_library_propagates_validator_success_for_absolute_path() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service
            .create_library("Movies".to_string(), "/media/videos/movies".to_string())
            .await;

        assert!(
            result.is_ok(),
            "a successful validation should be passed through: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_library_propagates_validator_success_for_relative_path() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service
            .create_library("Movies".to_string(), "movies".to_string())
            .await;

        assert!(
            result.is_ok(),
            "a successful validation should be passed through: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_library_propagates_a_path_outside_the_root() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::path_outside_root(
                "path escapes root",
            )),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service
            .create_library("Outside".to_string(), "/etc/secret".to_string())
            .await;

        assert!(matches!(result, Err(LibraryError::PathOutsideRoot(_))));
    }

    #[tokio::test]
    async fn test_create_library_propagates_validator_path_not_found_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::path_not_found("no such directory")),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            .expect_find_all()
            .times(1)
            .returning(|| Ok(vec![]));
        mock_library_repo
            .expect_create()
            .times(1)
            .returning(|_| Err(DbErr::Custom("insert failed".to_string())));

        let service = LocalLibraryService::new(
            Arc::new(mock_library_repo),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir.clone())),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service.get_library_files(lib_id.to_string()).await;

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|f| f.library_id == lib_id.to_string()));
        // Stored at `/media/videos/test.mp4` under a `/media/videos` root: the
        // listing carries the root-relative path, never the absolute one
        // (NFR-108).
        assert!(files.iter().all(|f| f.path == "test.mp4"));
    }

    #[tokio::test]
    async fn test_get_library_files_library_not_found_returns_library_not_found_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service.get_file_by_id(file_id.to_string()).await;

        assert!(result.is_ok());
        let opt = result.unwrap();
        // The delivery routes open this path, so it is the absolute one the
        // file is stored under -- unlike the client-facing listing.
        assert_eq!(
            opt,
            Some(LocatedFile {
                id: file_id,
                path: PathBuf::from("/media/videos/test.mp4"),
                mime_type: Some("video/mp4".to_string()),
            })
        );
    }

    #[tokio::test]
    async fn test_get_file_by_id_missing_file_returns_none() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service.get_file_by_id(Uuid::new_v4().to_string()).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_file_by_id_for_a_file_gone_missing_returns_none() {
        // The stream and download routes resolve a file through this lookup,
        // so a missing file (issue #179) answers 404 rather than an open on a
        // path that is not there.
        use beam_domain::repositories::FileRepository;

        let video_dir = PathBuf::from("/media/videos");
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let file_id = Uuid::new_v4();
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file_id, make_media_file(file_id, Uuid::new_v4()));
        file_repo
            .mark_missing(vec![file_id], chrono::Utc::now())
            .await
            .unwrap();
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            file_repo,
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        assert!(
            service
                .get_file_by_id(file_id.to_string())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_get_file_by_id_invalid_uuid_returns_invalid_id_error() {
        let video_dir = PathBuf::from("/media/videos");
        let service = LocalLibraryService::new(
            Arc::new(InMemoryLibraryRepository::default()),
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
            PathBuf::from("/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
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
        // No scan to cancel.
        let mut idle_index = MockIndexService::new();
        idle_index
            .expect_stop_scan()
            .returning(|id| stopped_scan(id, true));
        idle_index.expect_forget_library().return_const(());

        let service = LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            video_dir.clone(),
            PathBuf::from("/beam-data"),
            notif as Arc<dyn NotificationService>,
            Arc::new(idle_index),
            Arc::new(InMemoryPathValidator::success(video_dir)),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        );

        let result = service.delete_library(lib_id.to_string()).await;
        assert!(result.is_ok());
        assert!(result.unwrap());

        let events = notif_ref.published_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].message.contains("Movies"));
    }

    // ── root conflicts (issue #186) ──────────────────────────────────────────

    #[test]
    fn find_root_conflict_compares_whole_components_in_both_directions() {
        use crate::services::library::{RootConflict, find_root_conflict};

        let existing = vec![PathBuf::from("/m/movies"), PathBuf::from("/m/shows")];
        let data_dir = PathBuf::from("/srv/beam/data");
        let library = |p: &str| Some(RootConflict::Library(PathBuf::from(p)));
        let cases: [(&str, Option<RootConflict>); 9] = [
            ("/m/movies", library("/m/movies")),
            ("/m/movies/4k", library("/m/movies")),
            ("/m", library("/m/movies")),
            ("/m/movies2", None),
            ("/m/show", None),
            ("/m/anime", None),
            ("/srv/beam/data", Some(RootConflict::DataDir)),
            ("/srv/beam/data/artwork", Some(RootConflict::DataDir)),
            ("/srv", Some(RootConflict::DataDir)),
        ];
        for (candidate, expected) in cases {
            assert_eq!(
                find_root_conflict(std::path::Path::new(candidate), &existing, &data_dir),
                expected,
                "{candidate}"
            );
        }
    }

    fn conflict_service(
        lib_repo: Arc<InMemoryLibraryRepository>,
        resolves_to: &str,
    ) -> LocalLibraryService {
        LocalLibraryService::new(
            lib_repo,
            Arc::new(InMemoryFileRepository::default()),
            PathBuf::from("/media"),
            PathBuf::from("/media/beam-data"),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(MockIndexService::new()),
            Arc::new(InMemoryPathValidator::success(PathBuf::from(resolves_to))),
            Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        )
    }

    #[tokio::test]
    async fn a_root_nested_in_an_existing_library_is_rejected_and_nothing_is_stored() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        conflict_service(lib_repo.clone(), "/media/movies")
            .create_library("Movies".to_string(), "movies".to_string())
            .await
            .expect("the first library is disjoint");

        let err = conflict_service(lib_repo.clone(), "/media/movies/4k")
            .create_library("4K".to_string(), "movies/4k".to_string())
            .await
            .expect_err("nested inside Movies");

        assert!(
            matches!(err, LibraryError::PathOverlapsLibrary),
            "got {err:?}"
        );
        assert!(!err.to_string().contains('/'), "no path in {err}");
        assert_eq!(lib_repo.libraries.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_root_holding_the_data_directory_is_rejected() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());

        let err = conflict_service(lib_repo.clone(), "/media")
            .create_library("Everything".to_string(), ".".to_string())
            .await
            .expect_err("contains the data directory");

        assert!(
            matches!(err, LibraryError::PathOverlapsDataDir),
            "got {err:?}"
        );
        assert!(!err.to_string().contains('/'), "no path in {err}");
        assert!(lib_repo.libraries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sibling_sharing_a_name_prefix_is_accepted() {
        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        conflict_service(lib_repo.clone(), "/media/movies")
            .create_library("Movies".to_string(), "movies".to_string())
            .await
            .unwrap();

        conflict_service(lib_repo.clone(), "/media/movies2")
            .create_library("Movies 2".to_string(), "movies2".to_string())
            .await
            .expect("/media/movies2 is not inside /media/movies");

        assert_eq!(lib_repo.libraries.lock().unwrap().len(), 2);
    }

    // ── startup audit of stored roots (issue #186) ───────────────────────────

    /// Store a library directly, as one registered before overlaps were
    /// refused would be.
    async fn stored_library(lib_repo: &InMemoryLibraryRepository, name: &str, root: &str) -> Uuid {
        use beam_domain::models::CreateLibrary;
        use beam_domain::repositories::LibraryRepository;

        lib_repo
            .create(CreateLibrary {
                name: name.to_string(),
                description: None,
                root_path: PathBuf::from(root),
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_data_directory_moved_inside_an_existing_library_stops_startup() {
        use crate::services::library::{StartupRootError, audit_existing_roots};

        let lib_repo = InMemoryLibraryRepository::default();
        stored_library(&lib_repo, "Shows", "/media/shows").await;
        stored_library(&lib_repo, "Movies", "/media/movies").await;

        let err = audit_existing_roots(&lib_repo, std::path::Path::new("/media/movies/.beam"))
            .await
            .expect_err("the artwork cache would be written inside Movies");

        match &err {
            StartupRootError::DataDirOverlapsLibrary { library, root, .. } => {
                assert_eq!(library, "Movies");
                assert_eq!(root, &PathBuf::from("/media/movies"));
            }
            other => panic!("expected DataDirOverlapsLibrary, got {other:?}"),
        }
        assert!(
            err.to_string().contains("BEAM_DATA_DIR"),
            "the error names the setting to change: {err}"
        );
    }

    #[tokio::test]
    async fn a_data_directory_holding_a_library_stops_startup() {
        use crate::services::library::{StartupRootError, audit_existing_roots};

        let lib_repo = InMemoryLibraryRepository::default();
        stored_library(&lib_repo, "Movies", "/srv/beam/movies").await;

        let err = audit_existing_roots(&lib_repo, std::path::Path::new("/srv/beam"))
            .await
            .expect_err("the library lies inside the data directory");
        assert!(
            matches!(err, StartupRootError::DataDirOverlapsLibrary { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn stored_libraries_that_overlap_each_other_are_reported_not_refused() {
        use crate::services::library::{ExistingLibraryOverlap, audit_existing_roots};

        let lib_repo = InMemoryLibraryRepository::default();
        let movies = stored_library(&lib_repo, "Movies", "/media/movies").await;
        let four_k = stored_library(&lib_repo, "4K", "/media/movies/4k").await;
        stored_library(&lib_repo, "Movies 2", "/media/movies2").await;

        let overlaps = audit_existing_roots(&lib_repo, std::path::Path::new("/srv/beam"))
            .await
            .expect("an overlap between libraries does not stop startup");

        assert_eq!(
            overlaps.len(),
            1,
            "only Movies and 4K overlap: {overlaps:?}"
        );
        let ExistingLibraryOverlap { first, second } = overlaps[0].clone();
        let mut pair = [first, second];
        pair.sort();
        let mut expected = [movies, four_k];
        expected.sort();
        assert_eq!(pair, expected);
    }
}

// ── OsPathValidator: real containment ─────────────────────────────────────────
//
// The tests above drive LocalLibraryService through InMemoryPathValidator, which
// can only ever return the outcome the test configured. Directory containment --
// the actual security control -- lives in OsPathValidator, and is only meaningful
// against a real filesystem, so these use TempDir rather than a fake.
#[cfg(test)]
mod os_path_validator {
    use crate::services::library::{LibraryError, OsPathValidator, PathValidator};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// A root with `movies/` and `shows/` inside it. Returns the canonicalized root,
    /// because macOS puts TempDir under a symlinked /var and the validator returns
    /// canonical paths.
    fn root_with_children() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("temp dir");
        let root = tmp.path().canonicalize().expect("canonical root");
        fs::create_dir(root.join("movies")).expect("movies");
        fs::create_dir(root.join("shows")).expect("shows");
        (tmp, root)
    }

    fn validate(requested: &Path, root: &Path) -> Result<PathBuf, LibraryError> {
        OsPathValidator.validate_library_root(requested, root)
    }

    /// The rejection's message reaches an administrator's browser as the
    /// `detail` of a 400, so it must not name either the path that was asked
    /// for or the root it was checked against (NFR-108). The paths go to the
    /// log instead.
    fn assert_names_no_path(err: &LibraryError, paths: &[&Path]) {
        let message = err.to_string();
        for path in paths {
            let path = path.to_string_lossy();
            assert!(
                !message.contains(path.as_ref()),
                "a client-facing rejection must not carry a filesystem path; \
                 {message:?} names {path:?}"
            );
        }
        // Named paths are the ones this call knows about; the separator check
        // catches a message that grew a *different* path -- which is how these
        // messages regressed before (ADR-0011). The sibling helper in
        // `beam-index` makes the same assertion, and these two must not drift.
        assert!(
            !message.contains(std::path::MAIN_SEPARATOR),
            "a client-facing rejection must not carry any path component: {message:?}"
        );
    }

    #[test]
    fn absolute_path_inside_root_is_accepted_and_canonicalized() {
        let (_tmp, root) = root_with_children();
        let got = validate(&root.join("movies"), &root).expect("inside root");
        assert_eq!(got, root.join("movies"));
    }

    #[test]
    fn relative_path_is_resolved_against_root() {
        let (_tmp, root) = root_with_children();
        let got = validate(Path::new("movies"), &root).expect("inside root");
        assert_eq!(got, root.join("movies"));
    }

    #[test]
    fn root_itself_is_accepted() {
        let (_tmp, root) = root_with_children();
        let got = validate(&root, &root).expect("root is within root");
        assert_eq!(got, root);
    }

    #[test]
    fn dot_dot_traversal_out_of_root_is_rejected() {
        let (_tmp, root) = root_with_children();
        // Resolves to root's parent, which exists -- so this gets past the
        // canonicalize step and must be caught by the containment check itself.
        let err = validate(Path::new("movies/../.."), &root).expect_err("escapes root");
        assert!(
            matches!(err, LibraryError::PathOutsideRoot(_)),
            "expected Validation, got {err:?}"
        );
    }

    #[test]
    fn dot_dot_that_stays_inside_root_is_accepted() {
        let (_tmp, root) = root_with_children();
        let got = validate(Path::new("movies/../shows"), &root).expect("still inside root");
        assert_eq!(got, root.join("shows"));
    }

    #[test]
    fn absolute_path_outside_root_is_rejected() {
        let (_tmp, root) = root_with_children();
        let outside = TempDir::new().expect("other temp dir");
        let err = validate(outside.path(), &root).expect_err("outside root");
        assert!(
            matches!(err, LibraryError::PathOutsideRoot(_)),
            "expected Validation, got {err:?}"
        );
        assert_names_no_path(&err, &[&root, outside.path()]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pointing_outside_root_is_rejected() {
        let (_tmp, root) = root_with_children();
        let outside = TempDir::new().expect("other temp dir");
        let outside_real = outside.path().canonicalize().expect("canonical");
        std::os::unix::fs::symlink(&outside_real, root.join("escape")).expect("symlink");

        // The bare path is inside root; only canonicalization reveals the escape.
        // This is the case a naive starts_with on the *requested* path would miss.
        let err = validate(Path::new("escape"), &root).expect_err("symlink escapes root");
        assert!(
            matches!(err, LibraryError::PathOutsideRoot(_)),
            "expected Validation, got {err:?}"
        );
        // The resolved target is the one a naive message would print, and it
        // is exactly the location an administrator would rather not disclose.
        assert_names_no_path(&err, &[&root, &outside_real]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_staying_inside_root_is_accepted() {
        let (_tmp, root) = root_with_children();
        std::os::unix::fs::symlink(root.join("shows"), root.join("link")).expect("symlink");
        let got = validate(Path::new("link"), &root).expect("resolves inside root");
        assert_eq!(got, root.join("shows"));
    }

    #[test]
    fn sibling_directory_sharing_a_name_prefix_is_rejected() {
        // The classic prefix bug: "/videos-evil" starts with the *string* "/videos"
        // but is not inside it. Path::starts_with compares whole components, so this
        // is already correct -- the test exists to keep it that way.
        let tmp = TempDir::new().expect("temp dir");
        let base = tmp.path().canonicalize().expect("canonical");
        let root = base.join("videos");
        let evil = base.join("videos-evil");
        fs::create_dir(&root).expect("videos");
        fs::create_dir(&evil).expect("videos-evil");

        let err = validate(&evil, &root).expect_err("sibling is not inside root");
        assert!(
            matches!(err, LibraryError::PathOutsideRoot(_)),
            "expected Validation, got {err:?}"
        );
    }

    #[test]
    fn nonexistent_path_is_path_not_found_not_validation() {
        let (_tmp, root) = root_with_children();
        let err = validate(Path::new("no-such-dir"), &root).expect_err("does not exist");
        assert!(
            matches!(err, LibraryError::PathNotFound(_)),
            "expected PathNotFound, got {err:?}"
        );
    }

    #[test]
    fn nonexistent_root_is_path_not_found() {
        let tmp = TempDir::new().expect("temp dir");
        let missing_root = tmp.path().join("not-created");
        let err = validate(Path::new("anything"), &missing_root).expect_err("root missing");
        assert!(
            matches!(err, LibraryError::PathNotFound(_)),
            "expected PathNotFound, got {err:?}"
        );
        // This one used to be the root path verbatim, as the whole message.
        assert_names_no_path(&err, &[&missing_root]);
    }

    #[test]
    fn a_file_inside_root_is_rejected() {
        // Containment is not enough: a scan refuses a root that is not a
        // directory, so a regular file has to be refused at registration too --
        // otherwise the library is created and then fails every scan forever.
        let (_tmp, root) = root_with_children();
        let file = root.join("movies/note.txt");
        fs::write(&file, b"x").expect("write");
        let err = validate(Path::new("movies/note.txt"), &root).expect_err("not a directory");
        assert!(
            matches!(err, LibraryError::PathNotFound(_)),
            "expected PathNotFound, got {err:?}"
        );
        assert_names_no_path(&err, &[&root, &file]);
    }
}
