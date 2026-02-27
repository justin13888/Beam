#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::utils::{
        repository::{UserRepository, in_memory::InMemoryUserRepository},
        service::{AuthError, AuthService, LocalAuthService},
        session_store::{SessionStore, in_memory::InMemorySessionStore},
    };

    const TEST_JWT_SECRET: &str = "test-secret";

    fn build_service() -> (
        Arc<LocalAuthService>,
        Arc<InMemoryUserRepository>,
        Arc<InMemorySessionStore>,
    ) {
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let session_store = Arc::new(InMemorySessionStore::default());
        let svc = Arc::new(LocalAuthService::new(
            user_repo.clone(),
            session_store.clone(),
            TEST_JWT_SECRET.to_string(),
        ));
        (svc, user_repo, session_store)
    }

    // ─── register ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn register_unique_credentials_returns_auth_response() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "alice",
                "alice@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        assert!(!resp.token.is_empty());
        assert!(!resp.session_id.is_empty());
        assert_eq!(resp.user.username, "alice");
        assert_eq!(resp.user.email, "alice@example.com");
    }

    #[tokio::test]
    async fn register_creates_user_in_repository() {
        let (svc, user_repo, _) = build_service();
        svc.register(
            "bob",
            "bob@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let user = user_repo.find_by_username("bob").await.unwrap();
        assert!(user.is_some());
        assert_eq!(user.unwrap().username, "bob");
    }

    #[tokio::test]
    async fn register_stores_session_in_session_store() {
        let (svc, _, session_store) = build_service();
        let resp = svc
            .register(
                "carol",
                "carol@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let session = session_store.get(&resp.session_id).await.unwrap();
        assert!(session.is_some());
    }

    #[tokio::test]
    async fn register_duplicate_username_returns_error() {
        let (svc, _, _) = build_service();
        svc.register(
            "dave",
            "dave@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let result = svc
            .register(
                "dave",
                "dave2@example.com",
                "password456",
                "device-hash",
                "127.0.0.1",
            )
            .await;
        assert!(
            matches!(result, Err(AuthError::UserAlreadyExists)),
            "expected UserAlreadyExists, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn register_duplicate_email_returns_error() {
        let (svc, _, _) = build_service();
        svc.register(
            "eve",
            "shared@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let result = svc
            .register(
                "eve2",
                "shared@example.com",
                "password456",
                "device-hash",
                "127.0.0.1",
            )
            .await;
        assert!(
            matches!(result, Err(AuthError::UserAlreadyExists)),
            "expected UserAlreadyExists, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn register_password_stored_as_hash_not_plaintext() {
        let (svc, user_repo, _) = build_service();
        let raw_password = "supersecret";
        svc.register(
            "frank",
            "frank@example.com",
            raw_password,
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let user = user_repo
            .find_by_username("frank")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            user.password_hash, raw_password,
            "password should not be stored as plaintext"
        );
        assert!(
            user.password_hash.starts_with("$argon2"),
            "password should be an argon2 hash"
        );
    }

    // ─── login ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn login_with_correct_username_and_password_succeeds() {
        let (svc, _, _) = build_service();
        svc.register(
            "grace",
            "grace@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let resp = svc
            .login("grace", "password123", "device-hash", "127.0.0.1")
            .await
            .unwrap();
        assert!(!resp.token.is_empty());
        assert_eq!(resp.user.username, "grace");
    }

    #[tokio::test]
    async fn login_with_correct_email_and_password_succeeds() {
        let (svc, _, _) = build_service();
        svc.register(
            "henry",
            "henry@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let resp = svc
            .login(
                "henry@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        assert!(!resp.token.is_empty());
        assert_eq!(resp.user.username, "henry");
    }

    #[tokio::test]
    async fn login_with_wrong_password_returns_invalid_credentials() {
        let (svc, _, _) = build_service();
        svc.register(
            "iris",
            "iris@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let result = svc
            .login("iris", "wrongpassword", "device-hash", "127.0.0.1")
            .await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "expected InvalidCredentials, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn login_unknown_username_returns_invalid_credentials() {
        let (svc, _, _) = build_service();
        let result = svc
            .login("nonexistent", "password123", "device-hash", "127.0.0.1")
            .await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "expected InvalidCredentials, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn login_stores_new_session_in_session_store() {
        let (svc, _, session_store) = build_service();
        svc.register(
            "jake",
            "jake@example.com",
            "password123",
            "device-hash",
            "127.0.0.1",
        )
        .await
        .unwrap();
        let resp = svc
            .login("jake", "password123", "device-hash", "127.0.0.1")
            .await
            .unwrap();
        let session = session_store.get(&resp.session_id).await.unwrap();
        assert!(session.is_some());
    }

    // ─── verify_token ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn verify_token_freshly_issued_returns_authenticated_user() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "kate",
                "kate@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let auth_user = svc.verify_token(&resp.token).await.unwrap();
        assert_eq!(auth_user.user_id, resp.user.id);
    }

    #[tokio::test]
    async fn verify_token_tampered_returns_error() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "leo",
                "leo@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        // Flip a character in the signature (last segment of the JWT)
        let last_dot = resp.token.rfind('.').unwrap();
        let mut chars: Vec<char> = resp.token.chars().collect();
        let sig_pos = last_dot + 1;
        chars[sig_pos] = if chars[sig_pos] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        let result = svc.verify_token(&tampered).await;
        assert!(result.is_err(), "tampered token should not verify");
    }

    #[tokio::test]
    async fn verify_token_after_session_deleted_returns_error() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "mia",
                "mia@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        svc.logout(&resp.session_id).await.unwrap();
        let result = svc.verify_token(&resp.token).await;
        assert!(result.is_err(), "token should fail after session revocation");
    }

    #[tokio::test]
    async fn verify_token_signed_with_different_secret_returns_error() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "noah",
                "noah@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        // A second service with a different JWT secret cannot verify tokens from the first
        let svc2 = LocalAuthService::new(
            Arc::new(InMemoryUserRepository::default()),
            Arc::new(InMemorySessionStore::default()),
            "different-secret".to_string(),
        );
        let result = svc2.verify_token(&resp.token).await;
        assert!(
            result.is_err(),
            "token signed with different secret should not verify"
        );
    }

    // ─── refresh ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn refresh_valid_session_returns_new_auth_response() {
        let (svc, _, _) = build_service();
        let resp = svc
            .register(
                "olivia",
                "olivia@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let refreshed = svc.refresh(&resp.session_id).await.unwrap();
        assert!(!refreshed.token.is_empty());
        assert_eq!(refreshed.session_id, resp.session_id);
        assert_eq!(refreshed.user.username, "olivia");
    }

    #[tokio::test]
    async fn refresh_unknown_session_returns_error() {
        let (svc, _, _) = build_service();
        let result = svc.refresh("non-existent-session-id").await;
        assert!(result.is_err(), "refresh with unknown session should fail");
    }

    // ─── logout ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn logout_valid_session_removes_session_from_store() {
        let (svc, _, session_store) = build_service();
        let resp = svc
            .register(
                "peter",
                "peter@example.com",
                "password123",
                "device-hash",
                "127.0.0.1",
            )
            .await
            .unwrap();
        svc.logout(&resp.session_id).await.unwrap();
        let session = session_store.get(&resp.session_id).await.unwrap();
        assert!(session.is_none(), "session should be removed after logout");
    }

    #[tokio::test]
    async fn logout_unknown_session_is_idempotent() {
        let (svc, _, _) = build_service();
        let result = svc.logout("non-existent-session-id").await;
        assert!(
            result.is_ok(),
            "logout of unknown session should be a no-op, got: {result:?}"
        );
    }

    // ─── logout_all ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn logout_all_removes_all_sessions_for_user() {
        let (svc, _, session_store) = build_service();
        // Register creates session 1; a second login creates session 2
        let reg_resp = svc
            .register(
                "quinn",
                "quinn@example.com",
                "password123",
                "device-hash-1",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let login_resp = svc
            .login(
                "quinn",
                "password123",
                "device-hash-2",
                "127.0.0.2",
            )
            .await
            .unwrap();

        assert!(session_store.get(&reg_resp.session_id).await.unwrap().is_some());
        assert!(session_store.get(&login_resp.session_id).await.unwrap().is_some());

        let count = svc.logout_all(&reg_resp.user.id).await.unwrap();
        assert_eq!(count, 2, "should have deleted both sessions");

        assert!(
            session_store.get(&reg_resp.session_id).await.unwrap().is_none(),
            "first session should be gone"
        );
        assert!(
            session_store.get(&login_resp.session_id).await.unwrap().is_none(),
            "second session should be gone"
        );
    }
}
