/// Subcutaneous JWT auth tests for the GraphQL layer.
///
/// These tests verify that the `AuthGuard` and `AdminGuard` work correctly
/// when using JWT tokens for authentication — without requiring any external
/// infrastructure (no Redis, no PostgreSQL).
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_graphql::Request;
    use beam_auth::utils::{
        repository::in_memory::InMemoryUserRepository,
        service::{AuthService, LocalAuthService},
        session_store::in_memory::InMemorySessionStore,
    };

    use crate::graphql::create_schema;
    use crate::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
    use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
    use crate::services::hash::HashService;
    use crate::services::library::{LibraryError, LibraryService};
    use crate::services::metadata::{MetadataConfig, StubMetadataService};
    use crate::services::notification::InMemoryNotificationService;
    use crate::services::transcode::TranscodeService;
    use crate::state::{AppContext, AppServices, AppState, UserContext};

    // ─── Stub implementations for services not exercised during auth tests ───

    #[derive(Debug)]
    struct StubHashService;

    #[async_trait::async_trait]
    impl HashService for StubHashService {
        fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
            unimplemented!("not called in auth tests")
        }
        async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
            unimplemented!("not called in auth tests")
        }
    }

    #[derive(Debug)]
    struct StubLibraryService;

    #[async_trait::async_trait]
    impl LibraryService for StubLibraryService {
        async fn get_libraries(
            &self,
            _user_id: String,
        ) -> Result<Vec<crate::models::Library>, LibraryError> {
            unimplemented!("not called in auth tests")
        }
        async fn get_library_by_id(
            &self,
            _library_id: String,
        ) -> Result<Option<crate::models::Library>, LibraryError> {
            unimplemented!("not called in auth tests")
        }
        async fn get_library_files(
            &self,
            _library_id: String,
        ) -> Result<Vec<crate::models::LibraryFile>, LibraryError> {
            unimplemented!("not called in auth tests")
        }
        async fn create_library(
            &self,
            _name: String,
            _root_path: String,
        ) -> Result<crate::models::Library, LibraryError> {
            unimplemented!("not called in auth tests")
        }
        async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
            unimplemented!("not called in auth tests")
        }
        async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
            unimplemented!("not called in auth tests")
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
            unimplemented!("not called in auth tests")
        }
    }

    // ─── Test helpers ────────────────────────────────────────────────────────

    const TEST_JWT_SECRET: &str = "test-jwt-secret-for-auth-tests";

    struct TestContext {
        state: AppState,
        auth: Arc<LocalAuthService>,
        user_repo: Arc<InMemoryUserRepository>,
    }

    fn build_test_context() -> TestContext {
        let session_store = Arc::new(InMemorySessionStore::default());
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store,
            TEST_JWT_SECRET.to_string(),
        ));

        let notification = Arc::new(InMemoryNotificationService::new());
        let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        )));

        let services = AppServices {
            auth: auth.clone(),
            hash: Arc::new(StubHashService),
            library: Arc::new(StubLibraryService),
            metadata: Arc::new(StubMetadataService::new(MetadataConfig {
                cache_dir: PathBuf::from("/tmp"),
            })),
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

        TestContext {
            state,
            auth,
            user_repo,
        }
    }

    // ─── Tests ───────────────────────────────────────────────────────────────

    /// A request without an Authorization header should be rejected by `AuthGuard`.
    #[tokio::test]
    async fn test_auth_guard_rejects_unauthenticated_request() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        // AppContext with no user — simulates a request with no Bearer token
        let app_context = AppContext::new(None);
        let request = Request::new("{ adminEvents { id } }").data(app_context);

        let response = schema.execute(request).await;
        assert!(
            !response.errors.is_empty(),
            "expected auth error but got none"
        );
        assert!(
            response
                .errors
                .iter()
                .any(|e| e.message.contains("Unauthorized")
                    || e.message.to_lowercase().contains("unauthorized")),
            "expected Unauthorized error, got: {:?}",
            response.errors
        );
    }

    /// A request with a valid JWT should be accepted by `AuthGuard`.
    #[tokio::test]
    async fn test_auth_guard_accepts_valid_jwt() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        // Register a user and get a token
        let auth_resp = ctx
            .auth
            .register(
                "alice",
                "alice@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed");

        // Verify the token to get the AuthenticatedUser (simulating the HTTP handler)
        let authenticated = ctx
            .auth
            .verify_token(&auth_resp.token)
            .await
            .expect("token should be valid");

        let app_context = AppContext::new(Some(UserContext {
            user_id: authenticated.user_id,
        }));
        let request = Request::new("{ adminEvents { id } }").data(app_context);

        let response = schema.execute(request).await;
        assert!(
            response.errors.is_empty(),
            "expected no errors but got: {:?}",
            response.errors
        );
    }

    /// An expired or tampered JWT should result in a missing user context,
    /// which `AuthGuard` rejects.
    #[tokio::test]
    async fn test_auth_guard_rejects_invalid_jwt() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        // Attempt to verify a fabricated token — verify_token should fail,
        // so user_context stays None (the same path the HTTP handler takes)
        let verify_result = ctx.auth.verify_token("not.a.real.jwt").await;
        assert!(verify_result.is_err(), "invalid token should not verify");

        // Simulate the handler: failed verification → no UserContext
        let app_context = AppContext::new(None);
        let request = Request::new("{ adminEvents { id } }").data(app_context);

        let response = schema.execute(request).await;
        assert!(
            !response.errors.is_empty(),
            "expected auth error but got none"
        );
    }

    /// `AdminGuard` should reject a regular (non-admin) authenticated user.
    #[tokio::test]
    async fn test_admin_guard_rejects_non_admin_user() {
        let ctx = build_test_context();
        let schema = create_schema(ctx.state);

        let auth_resp = ctx
            .auth
            .register(
                "bob",
                "bob@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed");

        let authenticated = ctx
            .auth
            .verify_token(&auth_resp.token)
            .await
            .expect("token should be valid");

        // Bob is not an admin (is_admin defaults to false on register)
        let app_context = AppContext::new(Some(UserContext {
            user_id: authenticated.user_id,
        }));
        let request = Request::new("{ logs { id } }").data(app_context);

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

    /// `AdminGuard` should allow an admin user to access admin-only resolvers.
    #[tokio::test]
    async fn test_admin_guard_accepts_admin_user() {
        let ctx = build_test_context();

        // Manually insert an admin user into the in-memory repository
        use beam_auth::utils::models::CreateUser;
        use beam_auth::utils::repository::UserRepository;

        let password_hash = "$argon2id$v=19$m=19456,t=2,p=1$dummysalt$dummyhash".to_string();
        let admin_user = ctx
            .user_repo
            .create(CreateUser {
                username: "admin_carol".to_string(),
                email: "carol@example.com".to_string(),
                password_hash,
                is_admin: true,
            })
            .await
            .expect("should create admin user");

        // Manually build an AppContext for the admin
        let app_context = AppContext::new(Some(UserContext {
            user_id: admin_user.id.to_string(),
        }));

        let schema = create_schema(ctx.state);
        let request =
            Request::new("{ logs(limit: 10, offset: 0) { id message } }").data(app_context);

        let response = schema.execute(request).await;
        assert!(
            response.errors.is_empty(),
            "admin user should be allowed; got errors: {:?}",
            response.errors
        );
    }

    /// Verifying a token after session revocation (logout) should fail.
    #[tokio::test]
    async fn test_verify_token_fails_after_logout() {
        let ctx = build_test_context();

        let auth_resp = ctx
            .auth
            .register(
                "dave",
                "dave@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .expect("registration should succeed");

        // Token valid before logout
        ctx.auth
            .verify_token(&auth_resp.token)
            .await
            .expect("token should be valid before logout");

        // Revoke the session
        ctx.auth
            .logout(&auth_resp.session_id)
            .await
            .expect("logout should succeed");

        // Token should now be invalid (session no longer exists)
        let result = ctx.auth.verify_token(&auth_resp.token).await;
        assert!(
            result.is_err(),
            "token should be invalid after session revocation"
        );
    }
}
