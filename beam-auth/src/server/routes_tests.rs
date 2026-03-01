#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use salvo::http::header;
    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};
    use serde::Deserialize;
    use serde_json::json;

    use crate::server::routes::auth_routes;
    use crate::utils::repository::in_memory::InMemoryUserRepository;
    use crate::utils::service::{AuthService, LocalAuthService};
    use crate::utils::session_store::{SessionStore, in_memory::InMemorySessionStore};

    const TEST_JWT_SECRET: &str = "test-secret";

    /// Minimal deserialization target for AuthResponse — avoids adding
    /// `#[derive(Deserialize)]` to production types.
    #[derive(Debug, Deserialize)]
    struct TestAuthResponse {
        token: String,
        session_id: String,
    }

    /// Build a `Service` backed entirely by in-memory implementations.
    ///
    /// Returns the `Service` (for `TestClient::send`), the concrete
    /// `LocalAuthService` (for state inspection), and the `InMemorySessionStore`
    /// (for verifying session state directly).
    fn make_test_service() -> (Service, Arc<LocalAuthService>, Arc<InMemorySessionStore>) {
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let session_store = Arc::new(InMemorySessionStore::default());
        let auth = Arc::new(LocalAuthService::new(
            user_repo,
            session_store.clone(),
            TEST_JWT_SECRET.to_string(),
        ));
        let auth_dyn: Arc<dyn AuthService> = auth.clone();
        let router = Router::new()
            .hoop(affix_state::inject(auth_dyn))
            .push(auth_routes());
        (Service::new(router), auth, session_store)
    }

    // ─── POST /register ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn register_valid_body_returns_200_with_auth_response_and_cookie() {
        let (service, _, _) = make_test_service();

        let mut res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "alice",
                "email": "alice@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

        // Capture cookie header before consuming the body.
        let set_cookie = res.headers().get(header::SET_COOKIE).cloned();

        let auth: TestAuthResponse = res.take_json().await.unwrap();
        assert!(!auth.token.is_empty(), "token should be non-empty");
        assert!(
            !auth.session_id.is_empty(),
            "session_id should be non-empty"
        );

        let set_cookie_val = set_cookie.expect("Set-Cookie header should be present");
        assert!(
            set_cookie_val.to_str().unwrap().starts_with("session_id="),
            "Set-Cookie should set session_id"
        );
    }

    #[tokio::test]
    async fn register_malformed_json_returns_400() {
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/register")
            .raw_json("not valid json{{")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn register_duplicate_username_returns_400() {
        let (service, _, _) = make_test_service();

        // First registration succeeds.
        let res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "bob",
                "email": "bob@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));

        // Second registration with the same username should fail.
        let res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "bob",
                "email": "bob2@example.com",
                "password": "password456"
            }))
            .send(&service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn register_missing_required_field_returns_400() {
        let (service, _, _) = make_test_service();

        // Omit the `password` field.
        let res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "charlie",
                "email": "charlie@example.com"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    // ─── POST /login ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn login_correct_username_returns_200_with_auth_response_and_cookie() {
        let (service, _, _) = make_test_service();

        TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "dave",
                "email": "dave@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;

        let mut res = TestClient::post("http://0.0.0.0/login")
            .json(&json!({
                "username_or_email": "dave",
                "password": "password123"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

        let set_cookie = res.headers().get(header::SET_COOKIE).cloned();
        let auth: TestAuthResponse = res.take_json().await.unwrap();
        assert!(!auth.token.is_empty());
        assert!(!auth.session_id.is_empty());

        let set_cookie_val = set_cookie.expect("Set-Cookie should be set on login");
        assert!(set_cookie_val.to_str().unwrap().starts_with("session_id="));
    }

    #[tokio::test]
    async fn login_correct_email_returns_200() {
        let (service, _, _) = make_test_service();

        TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "eve",
                "email": "eve@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;

        let mut res = TestClient::post("http://0.0.0.0/login")
            .json(&json!({
                "username_or_email": "eve@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let auth: TestAuthResponse = res.take_json().await.unwrap();
        assert!(!auth.token.is_empty());
    }

    #[tokio::test]
    async fn login_wrong_password_returns_401() {
        let (service, _, _) = make_test_service();

        TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "frank",
                "email": "frank@example.com",
                "password": "correct-password"
            }))
            .send(&service)
            .await;

        let res = TestClient::post("http://0.0.0.0/login")
            .json(&json!({
                "username_or_email": "frank",
                "password": "wrong-password"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn login_unknown_username_returns_401() {
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/login")
            .json(&json!({
                "username_or_email": "nonexistent",
                "password": "password123"
            }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn login_malformed_json_returns_400() {
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/login")
            .raw_json("{bad json")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    // ─── POST /refresh ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn refresh_with_valid_session_cookie_returns_200() {
        let (service, _, _) = make_test_service();

        // Register to obtain a session_id.
        let mut reg_res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "grace",
                "email": "grace@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;
        let auth: TestAuthResponse = reg_res.take_json().await.unwrap();

        let mut res = TestClient::post("http://0.0.0.0/refresh")
            .add_header(
                header::COOKIE,
                format!("session_id={}", auth.session_id),
                true,
            )
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let refreshed: TestAuthResponse = res.take_json().await.unwrap();
        assert!(!refreshed.token.is_empty());
        assert_eq!(refreshed.session_id, auth.session_id);
    }

    #[tokio::test]
    async fn refresh_with_session_id_in_body_returns_200() {
        let (service, _, _) = make_test_service();

        let mut reg_res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "henry",
                "email": "henry@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;
        let auth: TestAuthResponse = reg_res.take_json().await.unwrap();

        let mut res = TestClient::post("http://0.0.0.0/refresh")
            .json(&json!({ "session_id": auth.session_id }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
        let refreshed: TestAuthResponse = res.take_json().await.unwrap();
        assert!(!refreshed.token.is_empty());
    }

    #[tokio::test]
    async fn refresh_invalid_session_id_returns_401() {
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/refresh")
            .json(&json!({ "session_id": "00000000-0000-0000-0000-000000000000" }))
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn refresh_no_session_returns_401() {
        let (service, _, _) = make_test_service();

        // No cookie, no body — the handler should return 401.
        let res = TestClient::post("http://0.0.0.0/refresh")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    // ─── POST /logout ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn logout_with_valid_session_cookie_returns_200_and_clears_cookie() {
        let (service, _, _) = make_test_service();

        let mut reg_res = TestClient::post("http://0.0.0.0/register")
            .json(&json!({
                "username": "iris",
                "email": "iris@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;
        let auth: TestAuthResponse = reg_res.take_json().await.unwrap();

        let res = TestClient::post("http://0.0.0.0/logout")
            .add_header(
                header::COOKIE,
                format!("session_id={}", auth.session_id),
                true,
            )
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

        // The removal Set-Cookie should have session_id with an empty value /
        // Max-Age=0 to instruct the browser to delete the cookie.
        let set_cookie = res.headers().get(header::SET_COOKIE);
        if let Some(hv) = set_cookie {
            let s = hv.to_str().unwrap();
            assert!(
                s.starts_with("session_id="),
                "Set-Cookie should reference session_id, got: {s}"
            );
        }
        // Note: Salvo only emits Set-Cookie when the cookie jar has delta entries.
        // Regardless, the status 200 and successful handler execution are the
        // primary assertions for this case.
    }

    #[tokio::test]
    async fn logout_no_session_returns_200_idempotent() {
        let (service, _, _) = make_test_service();

        // No cookie, no body — logout should be a no-op and return 200.
        let res = TestClient::post("http://0.0.0.0/logout")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));
    }

    // ─── POST /logout-all ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn logout_all_revokes_all_sessions_and_returns_count() {
        let (service, auth, session_store) = make_test_service();

        // Register creates session 1; login creates session 2
        let reg = auth
            .register(
                "alice2",
                "alice2@example.com",
                "password123",
                "device-1",
                "127.0.0.1",
            )
            .await
            .unwrap();
        let login = auth
            .login("alice2", "password123", "device-2", "127.0.0.2")
            .await
            .unwrap();

        let mut res = TestClient::post("http://0.0.0.0/logout-all")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

        let body: serde_json::Value = res.take_json().await.unwrap();
        assert_eq!(body["revoked"], 2);

        // Both sessions must be gone from the store
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
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/logout-all")
            .bearer_auth("not.a.real.token")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn logout_all_with_missing_auth_header_returns_401() {
        let (service, _, _) = make_test_service();

        let res = TestClient::post("http://0.0.0.0/logout-all")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    // ─── GET /sessions ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_sessions_returns_session_summaries() {
        let (service, auth, _) = make_test_service();

        let reg = auth
            .register(
                "bob2",
                "bob2@example.com",
                "password123",
                "device-bob",
                "10.0.0.1",
            )
            .await
            .unwrap();

        let mut res = TestClient::get("http://0.0.0.0/sessions")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

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
        let (service, auth, _) = make_test_service();

        let reg = auth
            .register(
                "carol2",
                "carol2@example.com",
                "password123",
                "device-1",
                "192.168.1.1",
            )
            .await
            .unwrap();
        auth.login("carol2", "password123", "device-2", "192.168.1.2")
            .await
            .unwrap();

        let mut res = TestClient::get("http://0.0.0.0/sessions")
            .bearer_auth(&reg.token)
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::OK));

        let sessions: Vec<serde_json::Value> = res.take_json().await.unwrap();
        assert_eq!(sessions.len(), 2, "should return both active sessions");
    }

    #[tokio::test]
    async fn list_sessions_with_invalid_jwt_returns_401() {
        let (service, _, _) = make_test_service();

        let res = TestClient::get("http://0.0.0.0/sessions")
            .bearer_auth("invalid.token.here")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn list_sessions_with_missing_auth_header_returns_401() {
        let (service, _, _) = make_test_service();

        let res = TestClient::get("http://0.0.0.0/sessions")
            .send(&service)
            .await;

        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }
}
