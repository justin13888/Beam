#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};

    use crate::server::routes::auth_routes;
    use crate::utils::{
        repository::in_memory::InMemoryUserRepository,
        service::{AuthService, LocalAuthService},
        session_store::{SessionStore, in_memory::InMemorySessionStore},
    };

    const TEST_JWT_SECRET: &str = "test-secret";

    fn build_test_service() -> (Service, Arc<LocalAuthService>, Arc<InMemorySessionStore>) {
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let session_store = Arc::new(InMemorySessionStore::default());
        let auth_local = Arc::new(LocalAuthService::new(
            user_repo,
            session_store.clone(),
            TEST_JWT_SECRET.to_string(),
        ));
        let auth_dyn: Arc<dyn AuthService> = auth_local.clone();

        let router = Router::new()
            .hoop(affix_state::inject(auth_dyn))
            .push(Router::with_path("v1/auth").push(auth_routes()));

        (Service::new(router), auth_local, session_store)
    }

    // ─── POST /logout-all ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn logout_all_revokes_all_sessions_and_returns_count() {
        let (service, auth, session_store) = build_test_service();

        // Register creates session 1; login creates session 2
        let reg = auth
            .register(
                "alice",
                "alice@example.com",
                "password123",
                "device-1",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let login = auth
            .login("alice", "password123", "device-2", "127.0.0.2")
            .await
            .unwrap();

        let mut res = TestClient::post("http://localhost/v1/auth/logout-all")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::OK);

        let body: serde_json::Value = res.take_json().await.unwrap();
        assert_eq!(body["revoked"], 2);

        // Both sessions must be gone
        assert!(
            session_store.get(&reg.session_id).await.unwrap().is_none(),
            "first session should be removed"
        );
        assert!(
            session_store
                .get(&login.session_id)
                .await
                .unwrap()
                .is_none(),
            "second session should be removed"
        );
    }

    #[tokio::test]
    async fn logout_all_with_invalid_jwt_returns_401() {
        let (service, _, _) = build_test_service();

        let res = TestClient::post("http://localhost/v1/auth/logout-all")
            .bearer_auth("not.a.real.token")
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_all_with_missing_auth_header_returns_401() {
        let (service, _, _) = build_test_service();

        let res = TestClient::post("http://localhost/v1/auth/logout-all")
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

    // ─── GET /sessions ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_sessions_returns_session_summaries() {
        let (service, auth, _) = build_test_service();

        let reg = auth
            .register(
                "bob",
                "bob@example.com",
                "password123",
                "device-bob",
                "10.0.0.1",
            )
            .await
            .unwrap();

        let mut res = TestClient::get("http://localhost/v1/auth/sessions")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::OK);

        let sessions: Vec<serde_json::Value> = res.take_json().await.unwrap();
        assert!(!sessions.is_empty(), "should have at least one session");

        let s = &sessions[0];
        assert!(s["session_id"].is_string(), "session_id should be a string");
        assert!(
            s["device_hash"].is_string(),
            "device_hash should be a string"
        );
        assert!(s["ip"].is_string(), "ip should be a string");
        assert!(s["created_at"].is_number(), "created_at should be a number");
        assert!(
            s["last_active"].is_number(),
            "last_active should be a number"
        );
    }

    #[tokio::test]
    async fn list_sessions_returns_all_active_sessions() {
        let (service, auth, _) = build_test_service();

        let reg = auth
            .register(
                "carol",
                "carol@example.com",
                "password123",
                "device-1",
                "192.168.1.1",
            )
            .await
            .unwrap();
        auth.login("carol", "password123", "device-2", "192.168.1.2")
            .await
            .unwrap();

        let mut res = TestClient::get("http://localhost/v1/auth/sessions")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::OK);

        let sessions: Vec<serde_json::Value> = res.take_json().await.unwrap();
        assert_eq!(sessions.len(), 2, "should return both active sessions");
    }

    #[tokio::test]
    async fn list_sessions_with_invalid_jwt_returns_401() {
        let (service, _, _) = build_test_service();

        let res = TestClient::get("http://localhost/v1/auth/sessions")
            .bearer_auth("invalid.token.here")
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_sessions_with_missing_auth_header_returns_401() {
        let (service, _, _) = build_test_service();

        let res = TestClient::get("http://localhost/v1/auth/sessions")
            .send(&service)
            .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }
}
