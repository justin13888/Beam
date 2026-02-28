/// Subcutaneous resolver tests for the GraphQL layer.
///
/// These tests verify that the library, media (search), and admin resolvers
/// work correctly end-to-end using all in-memory service implementations —
/// without any external infrastructure (no Postgres, no Redis).
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_graphql::Request;
    use beam_auth::utils::{
        models::CreateUser,
        repository::{UserRepository, in_memory::InMemoryUserRepository},
        service::{AuthService, LocalAuthService},
        session_store::in_memory::InMemorySessionStore,
    };
    use beam_index::models::domain::movie::Movie;
    use beam_index::models::domain::{Library as DomainLibrary, Show};
    use beam_index::services::index::MockIndexService;
    use uuid::Uuid;

    use crate::graphql::create_schema;
    use crate::repositories::admin_log::AdminLogRepository;
    use crate::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::library::in_memory::InMemoryLibraryRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::library::{InMemoryPathValidator, LocalLibraryService};
    use crate::services::metadata::DbMetadataService;
    use crate::services::notification::{InMemoryNotificationService, NotificationService};
    use crate::services::transcode::TranscodeService;
    use crate::state::{AppContext, AppServices, AppState, UserContext};

    // ─── Stub implementations for services not exercised in resolver tests ────

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in resolver tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in resolver tests")
        }
    }

    #[derive(Debug)]
    struct StubTranscodeService;

    #[async_trait::async_trait]
    impl TranscodeService for StubTranscodeService {
        async fn generate_mp4_cache(
            &self,
            _source_path: &std::path::Path,
            _output_path: &std::path::Path,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            unimplemented!("not called in resolver tests")
        }
    }

    // ─── Test helpers ────────────────────────────────────────────────────────

    const TEST_JWT_SECRET: &str = "test-jwt-secret-for-resolver-tests";

    struct TestContext {
        state: AppState,
        auth: Arc<LocalAuthService>,
        user_repo: Arc<InMemoryUserRepository>,
        library_repo: Arc<InMemoryLibraryRepository>,
        movie_repo: Arc<InMemoryMovieRepository>,
        show_repo: Arc<InMemoryShowRepository>,
        admin_log_repo: Arc<InMemoryAdminLogRepository>,
        notification: Arc<InMemoryNotificationService>,
    }

    fn build_test_context() -> TestContext {
        // Default: mock index returns Ok(0) for any scan call
        let mut mock_index = MockIndexService::new();
        mock_index.expect_scan_library().returning(|_| Ok(0));
        build_test_context_with(mock_index)
    }

    fn build_test_context_with(index_service: MockIndexService) -> TestContext {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            TEST_JWT_SECRET.to_string(),
        ));

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());

        let library_service = Arc::new(LocalLibraryService::new(
            library_repo.clone(),
            file_repo.clone(),
            PathBuf::from("/tmp"),
            notification.clone(),
            Arc::new(index_service),
            Arc::new(InMemoryPathValidator::success(PathBuf::from("/tmp"))),
        ));

        let metadata_service = Arc::new(DbMetadataService::new(
            movie_repo.clone(),
            show_repo.clone(),
            file_repo,
            stream_repo,
        ));

        let admin_log: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo.clone()));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library: library_service,
            metadata: metadata_service,
            transcode: Arc::new(StubTranscodeService),
            notification: notification.clone(),
            admin_log,
            user_repo: user_repo.clone(),
        };

        let config = crate::config::ServerConfig {
            bind_address: "0.0.0.0:8000".to_string(),
            server_url: "http://localhost:8000".to_string(),
            enable_metrics: false,
            video_dir: PathBuf::from("/tmp"),
            cache_dir: PathBuf::from("/tmp"),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            jwt_secret: TEST_JWT_SECRET.to_string(),
            redis_url: "redis://localhost".to_string(),
            beam_index_url: "http://localhost:50051".to_string(),
        };

        TestContext {
            state: AppState::new(config, services),
            auth,
            user_repo,
            library_repo,
            movie_repo,
            show_repo,
            admin_log_repo,
            notification,
        }
    }

    /// Create an authenticated AppContext for a newly registered regular user.
    /// Takes auth service reference directly so it works even after ctx.state is moved.
    /// Provide a unique username to avoid conflicts when multiple users are needed in one test.
    async fn seed_regular_user(auth: &Arc<LocalAuthService>, username: &str) -> AppContext {
        let auth_resp = auth
            .register(
                username,
                &format!("{}@example.com", username),
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed");

        let authenticated = auth
            .verify_token(&auth_resp.token)
            .await
            .expect("token should be valid");

        AppContext::new(Some(UserContext {
            user_id: authenticated.user_id,
        }))
    }

    /// Create an authenticated AppContext for a newly-created admin user.
    /// Takes user_repo reference directly so it works even after ctx.state is moved.
    async fn seed_admin_user(user_repo: &Arc<InMemoryUserRepository>) -> AppContext {
        let password_hash = "$argon2id$v=19$m=19456,t=2,p=1$dummysalt$dummyhash".to_string();
        let admin_user = user_repo
            .create(CreateUser {
                username: "admin".to_string(),
                email: "admin@example.com".to_string(),
                password_hash,
                is_admin: true,
            })
            .await
            .expect("admin user creation should succeed");

        AppContext::new(Some(UserContext {
            user_id: admin_user.id.to_string(),
        }))
    }

    fn make_domain_movie(title: &str) -> Movie {
        Movie {
            id: Uuid::new_v4(),
            title: title.to_string(),
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
            rating_tmdb: None,
            rating_imdb: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_domain_show(title: &str) -> Show {
        Show {
            id: Uuid::new_v4(),
            title: title.to_string(),
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
        }
    }

    // ─── Library Resolver Tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_libraries_empty_returns_empty_list() {
        let ctx = build_test_context();
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let request = Request::new("{ libraries { id name } }").data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let libs = json["libraries"].as_array().unwrap();
        assert_eq!(libs.len(), 0, "expected empty library list");
    }

    #[tokio::test]
    async fn test_libraries_returns_seeded_libraries() {
        let ctx = build_test_context();

        // Seed 2 libraries directly in the in-memory repository
        let lib1 = DomainLibrary {
            id: Uuid::new_v4(),
            name: "Movies".to_string(),
            root_path: PathBuf::from("/tmp/movies"),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        };
        let lib2 = DomainLibrary {
            id: Uuid::new_v4(),
            name: "Shows".to_string(),
            root_path: PathBuf::from("/tmp/shows"),
            description: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_scan_started_at: None,
            last_scan_finished_at: None,
            last_scan_file_count: None,
        };
        ctx.library_repo
            .libraries
            .lock()
            .unwrap()
            .insert(lib1.id, lib1);
        ctx.library_repo
            .libraries
            .lock()
            .unwrap()
            .insert(lib2.id, lib2);

        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);
        let request = Request::new("{ libraries { id name } }").data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let libs = json["libraries"].as_array().unwrap();
        assert_eq!(libs.len(), 2, "expected 2 libraries");

        let names: Vec<&str> = libs.iter().map(|l| l["name"].as_str().unwrap()).collect();
        assert!(
            names.contains(&"Movies") && names.contains(&"Shows"),
            "expected both library names, got: {:?}",
            names
        );
    }

    #[tokio::test]
    async fn test_libraries_unauthenticated_returns_unauthorized() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        let app_ctx = AppContext::new(None);
        let request = Request::new("{ libraries { id name } }").data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected Unauthorized error but got none"
        );
        assert!(
            response
                .errors
                .iter()
                .any(|e| e.message.to_lowercase().contains("unauthorized")),
            "expected Unauthorized error, got: {:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn test_create_library_creates_and_returns_library() {
        let ctx = build_test_context();
        // Clone state so we can still access ctx.library_repo after schema creation
        let schema = create_schema(ctx.state.clone());
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;

        let query =
            r#"mutation { createLibrary(name: "My Movies", rootPath: "/tmp/movies") { id name } }"#;
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let library = &json["createLibrary"];
        assert_eq!(library["name"].as_str().unwrap(), "My Movies");
        assert!(
            library["id"].as_str().is_some(),
            "expected id to be present"
        );

        // Verify library persisted in the in-memory repository
        let libs = ctx.library_repo.libraries.lock().unwrap();
        assert_eq!(libs.len(), 1, "expected 1 library in repo");
        assert!(
            libs.values().any(|l| l.name == "My Movies"),
            "library should be in repo"
        );
    }

    #[tokio::test]
    async fn test_create_library_with_repo_failure_returns_error() {
        use crate::repositories::library::MockLibraryRepository;

        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            TEST_JWT_SECRET.to_string(),
        ));

        let mut mock_lib_repo = MockLibraryRepository::new();
        mock_lib_repo
            .expect_create()
            .returning(|_| Err(sea_orm::DbErr::Custom("simulated repo failure".to_string())));

        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notification = Arc::new(InMemoryNotificationService::new());
        let mut mock_index = MockIndexService::new();
        mock_index.expect_scan_library().returning(|_| Ok(0));

        let library_service = Arc::new(LocalLibraryService::new(
            Arc::new(mock_lib_repo),
            file_repo.clone(),
            PathBuf::from("/tmp"),
            notification.clone(),
            Arc::new(mock_index),
            Arc::new(InMemoryPathValidator::success(PathBuf::from("/tmp"))),
        ));

        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let admin_log: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo));

        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let metadata_service = Arc::new(DbMetadataService::new(
            movie_repo,
            show_repo,
            file_repo,
            stream_repo,
        ));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library: library_service,
            metadata: metadata_service,
            transcode: Arc::new(StubTranscodeService),
            notification,
            admin_log,
            user_repo: user_repo.clone(),
        };

        let config = crate::config::ServerConfig {
            bind_address: "0.0.0.0:8000".to_string(),
            server_url: "http://localhost:8000".to_string(),
            enable_metrics: false,
            video_dir: PathBuf::from("/tmp"),
            cache_dir: PathBuf::from("/tmp"),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            jwt_secret: TEST_JWT_SECRET.to_string(),
            redis_url: "redis://localhost".to_string(),
            beam_index_url: "http://localhost:50051".to_string(),
        };

        let state = AppState::new(config, services);
        let schema = create_schema(state);

        // Register a user to get a valid auth context
        let auth_resp = auth
            .register(
                "eve",
                "eve@example.com",
                "password123",
                "device",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let authenticated = auth.verify_token(&auth_resp.token).await.unwrap();
        let app_ctx = AppContext::new(Some(UserContext {
            user_id: authenticated.user_id,
        }));

        let query =
            r#"mutation { createLibrary(name: "Fail", rootPath: "/tmp/fail") { id name } }"#;
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected an error from repo failure but got none"
        );
    }

    #[tokio::test]
    async fn test_scan_library_returns_count() {
        let mut mock_index = MockIndexService::new();
        mock_index.expect_scan_library().returning(|_| Ok(5));

        let ctx = build_test_context_with(mock_index);
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let lib_id = Uuid::new_v4().to_string();
        let query = format!("mutation {{ scanLibrary(id: \"{}\") }}", lib_id);
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        assert_eq!(
            json["scanLibrary"].as_u64().unwrap(),
            5,
            "expected scan count of 5"
        );
    }

    #[tokio::test]
    async fn test_scan_library_propagates_error() {
        use beam_index::services::index::IndexError;

        let mut mock_index = MockIndexService::new();
        mock_index
            .expect_scan_library()
            .returning(|_| Err(IndexError::LibraryNotFound));

        let ctx = build_test_context_with(mock_index);
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let lib_id = Uuid::new_v4().to_string();
        let query = format!("mutation {{ scanLibrary(id: \"{}\") }}", lib_id);
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected an error from scan failure but got none"
        );
    }

    #[tokio::test]
    async fn test_delete_library_returns_true_and_removes_from_repo() {
        let ctx = build_test_context();
        // Clone state so we can use schema twice and still check library_repo
        let schema = create_schema(ctx.state.clone());

        let app_ctx_create = seed_regular_user(&ctx.auth, "alice").await;
        let app_ctx_delete = seed_regular_user(&ctx.auth, "bob").await;

        // Create a library via mutation first
        let create_query =
            r#"mutation { createLibrary(name: "ToDelete", rootPath: "/tmp/to-delete") { id } }"#;
        let create_response = schema
            .execute(Request::new(create_query).data(app_ctx_create))
            .await;
        assert!(
            create_response.errors.is_empty(),
            "createLibrary failed: {:?}",
            create_response.errors
        );
        let create_json = create_response.data.into_json().unwrap();
        let lib_id = create_json["createLibrary"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Verify library is in repo
        assert_eq!(
            ctx.library_repo.libraries.lock().unwrap().len(),
            1,
            "library should be in repo before delete"
        );

        // Delete it
        let delete_query = format!("mutation {{ deleteLibrary(id: \"{}\") }}", lib_id);
        let delete_response = schema
            .execute(Request::new(delete_query).data(app_ctx_delete))
            .await;
        assert!(
            delete_response.errors.is_empty(),
            "deleteLibrary failed: {:?}",
            delete_response.errors
        );
        let delete_json = delete_response.data.into_json().unwrap();
        assert!(
            delete_json["deleteLibrary"].as_bool().unwrap(),
            "deleteLibrary should return true"
        );

        // Verify library is removed from repo
        assert_eq!(
            ctx.library_repo.libraries.lock().unwrap().len(),
            0,
            "library should be removed from repo after delete"
        );
    }

    #[tokio::test]
    async fn test_delete_library_nonexistent_returns_error() {
        let ctx = build_test_context();
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let non_existent_id = Uuid::new_v4().to_string();
        let query = format!("mutation {{ deleteLibrary(id: \"{}\") }}", non_existent_id);
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected error for non-existent library delete but got none"
        );
    }

    // ─── Media Resolver Tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_search_empty_repos_returns_empty_connection() {
        let ctx = build_test_context();
        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let query = r#"{ search(first: 10) { edges { cursor } pageInfo { hasNextPage hasPreviousPage } } }"#;
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let edges = json["search"]["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 0, "expected empty edges");
        assert!(
            !json["search"]["pageInfo"]["hasNextPage"].as_bool().unwrap(),
            "hasNextPage should be false"
        );
    }

    #[tokio::test]
    async fn test_search_returns_two_movies() {
        let ctx = build_test_context();

        let m1 = make_domain_movie("Alpha Movie");
        let m2 = make_domain_movie("Beta Movie");
        ctx.movie_repo.movies.lock().unwrap().insert(m1.id, m1);
        ctx.movie_repo.movies.lock().unwrap().insert(m2.id, m2);

        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let query = r#"{ search(first: 10) { edges { cursor node { ... on MovieMetadata { title { original } } } } pageInfo { hasNextPage } } }"#;
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let edges = json["search"]["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2, "expected 2 edges");
        assert!(
            !json["search"]["pageInfo"]["hasNextPage"].as_bool().unwrap(),
            "hasNextPage should be false for 2 items with first: 10"
        );
    }

    #[tokio::test]
    async fn test_search_filter_movie_type_excludes_shows() {
        let ctx = build_test_context();

        let movie = make_domain_movie("The Movie");
        let show = make_domain_show("The Show");
        ctx.movie_repo
            .movies
            .lock()
            .unwrap()
            .insert(movie.id, movie);
        ctx.show_repo.shows.lock().unwrap().insert(show.id, show);

        let app_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let query = r#"{ search(first: 10, mediaType: MOVIE) { edges { cursor node { ... on MovieMetadata { title { original } } } } } }"#;
        let request = Request::new(query).data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let edges = json["search"]["edges"].as_array().unwrap();
        assert_eq!(
            edges.len(),
            1,
            "expected only 1 edge (movie), show filtered out"
        );
        let title = edges[0]["node"]["title"]["original"].as_str().unwrap();
        assert_eq!(title, "The Movie");
    }

    #[tokio::test]
    async fn test_search_pagination_first_one_has_next_page() {
        let ctx = build_test_context();

        // Seed 3 movies — they will be sorted alphabetically
        for title in &["Alpha", "Beta", "Gamma"] {
            let m = make_domain_movie(title);
            ctx.movie_repo.movies.lock().unwrap().insert(m.id, m);
        }

        // Clone state so we can use schema for two requests
        let schema = create_schema(ctx.state.clone());
        let app_ctx_p1 = seed_regular_user(&ctx.auth, "alice").await;
        let app_ctx_p2 = seed_regular_user(&ctx.auth, "bob").await;

        // First page: 1 item
        let query = r#"{ search(first: 1) { edges { cursor node { ... on MovieMetadata { title { original } } } } pageInfo { hasNextPage endCursor } } }"#;
        let response = schema.execute(Request::new(query).data(app_ctx_p1)).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let page1_edges = json["search"]["edges"].as_array().unwrap();
        assert_eq!(page1_edges.len(), 1, "expected 1 edge on first page");
        assert!(
            json["search"]["pageInfo"]["hasNextPage"].as_bool().unwrap(),
            "hasNextPage should be true"
        );

        // Use the cursor to get page 2
        let cursor = json["search"]["pageInfo"]["endCursor"]
            .as_str()
            .unwrap()
            .to_string();
        let page2_query = format!(
            r#"{{ search(first: 1, after: "{}") {{ edges {{ node {{ ... on MovieMetadata {{ title {{ original }} }} }} }} pageInfo {{ hasNextPage }} }} }}"#,
            cursor
        );
        let page2_response = schema
            .execute(Request::new(page2_query).data(app_ctx_p2))
            .await;

        assert!(
            page2_response.errors.is_empty(),
            "unexpected page2 errors: {:?}",
            page2_response.errors
        );
        let page2_json = page2_response.data.into_json().unwrap();
        let page2_edges = page2_json["search"]["edges"].as_array().unwrap();
        assert_eq!(page2_edges.len(), 1, "expected 1 edge on second page");
        // Second movie alphabetically should be "Beta"
        let title = page2_edges[0]["node"]["title"]["original"]
            .as_str()
            .unwrap();
        assert_eq!(title, "Beta", "second page should contain 'Beta'");
    }

    // ─── Admin Resolver Tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_logs_returns_entries_for_admin() {
        use beam_index::models::domain::{AdminLogCategory, AdminLogLevel, CreateAdminLog};

        let ctx = build_test_context();

        // Seed 2 log entries directly in the repository
        ctx.admin_log_repo
            .create(CreateAdminLog {
                level: AdminLogLevel::Info,
                category: AdminLogCategory::LibraryScan,
                message: "Scan started".to_string(),
                details: None,
            })
            .await
            .unwrap();
        ctx.admin_log_repo
            .create(CreateAdminLog {
                level: AdminLogLevel::Warning,
                category: AdminLogCategory::System,
                message: "Disk space low".to_string(),
                details: None,
            })
            .await
            .unwrap();

        let admin_ctx = seed_admin_user(&ctx.user_repo).await;
        let schema = create_schema(ctx.state);
        let request = Request::new("{ logs(limit: 10, offset: 0) { id message } }").data(admin_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let logs = json["logs"].as_array().unwrap();
        assert_eq!(logs.len(), 2, "expected 2 log entries");
    }

    #[tokio::test]
    async fn test_logs_forbidden_for_non_admin_user() {
        let ctx = build_test_context();
        let regular_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        let request =
            Request::new("{ logs(limit: 10, offset: 0) { id message } }").data(regular_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected Forbidden error but got none"
        );
        assert!(
            response
                .errors
                .iter()
                .any(|e| e.message.contains("admin") || e.message.contains("Forbidden")),
            "expected admin/Forbidden error, got: {:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn test_logs_unauthenticated_returns_unauthorized() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        let app_ctx = AppContext::new(None);
        let request = Request::new("{ logs(limit: 10, offset: 0) { id message } }").data(app_ctx);
        let response = schema.execute(request).await;

        assert!(
            !response.errors.is_empty(),
            "expected Unauthorized error but got none"
        );
        assert!(
            response
                .errors
                .iter()
                .any(|e| e.message.to_lowercase().contains("unauthorized")),
            "expected Unauthorized error, got: {:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn test_admin_events_query_returns_published_events() {
        use crate::services::notification::{AdminEvent, EventCategory};

        let ctx = build_test_context();

        // Publish an event via the notification service (trait method in scope via import)
        ctx.notification.publish(AdminEvent::info(
            EventCategory::System,
            "Test event published".to_string(),
            None,
            None,
        ));

        let regular_ctx = seed_regular_user(&ctx.auth, "alice").await;
        let schema = create_schema(ctx.state);

        // adminEvents query requires AuthGuard only (not AdminGuard)
        let request = Request::new("{ adminEvents(limit: 10) { id message } }").data(regular_ctx);
        let response = schema.execute(request).await;

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let json = response.data.into_json().unwrap();
        let events = json["adminEvents"].as_array().unwrap();
        assert_eq!(events.len(), 1, "expected 1 published event");
        assert_eq!(
            events[0]["message"].as_str().unwrap(),
            "Test event published"
        );
    }
}
