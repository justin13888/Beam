#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};
    use serde_json::Value;

    use crate::server::oidc_routes::{OidcRuntimeConfig, oidc_routes};
    use crate::utils::oidc::fake::FakeOidcClient;
    use crate::utils::oidc::{OidcClient, OidcIdentity};
    use crate::utils::pending_auth_store::PendingAuthStore;
    use crate::utils::pending_auth_store::in_memory::InMemoryPendingAuthStore;
    use crate::utils::repository::UserRepository;
    use crate::utils::repository::in_memory::InMemoryUserRepository;
    use crate::utils::session_store::SessionStore;
    use crate::utils::session_store::in_memory::InMemorySessionStore;

    fn test_config() -> OidcRuntimeConfig {
        OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_string(),
            cookie_secure: false,
            admin_emails_csv: "admin@beam.localhost".to_string(),
            session_idle_days: 14,
            session_max_days: 60,
        }
    }

    fn identity(email: &str, verified: bool) -> OidcIdentity {
        OidcIdentity {
            issuer: "https://dex.test".to_string(),
            subject: "subj-1".to_string(),
            email: Some(email.to_string()),
            email_verified: verified,
            name: Some("Test User".to_string()),
            picture: Some("https://dex.test/avatar.png".to_string()),
        }
    }

    struct Harness {
        service: Service,
        oidc_client: Arc<FakeOidcClient>,
        user_repo: Arc<InMemoryUserRepository>,
        session_store: Arc<InMemorySessionStore>,
    }

    fn make_harness(oidc_client: FakeOidcClient) -> Harness {
        let oidc_client = Arc::new(oidc_client);
        let oidc_dyn: Arc<dyn OidcClient> = oidc_client.clone();
        let pending_auth_store: Arc<dyn PendingAuthStore> =
            Arc::new(InMemoryPendingAuthStore::default());
        let session_store = Arc::new(InMemorySessionStore::default());
        let session_dyn: Arc<dyn SessionStore> = session_store.clone();
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let user_repo_dyn: Arc<dyn UserRepository> = user_repo.clone();

        let router = Router::new()
            .hoop(affix_state::inject(oidc_dyn))
            .hoop(affix_state::inject(pending_auth_store))
            .hoop(affix_state::inject(session_dyn))
            .hoop(affix_state::inject(user_repo_dyn))
            .hoop(affix_state::inject(test_config()))
            .push(oidc_routes());

        Harness {
            service: Service::new(router),
            oidc_client,
            user_repo,
            session_store,
        }
    }

    /// Drives `GET /login` and returns the `beam_oidc_state` cookie value and
    /// the redirect `Location` header.
    async fn do_login(harness: &Harness, redirect: Option<&str>) -> (String, String) {
        let url = match redirect {
            Some(r) => format!("http://0.0.0.0/login?redirect={r}"),
            None => "http://0.0.0.0/login".to_string(),
        };
        let res = TestClient::get(url).send(&harness.service).await;
        assert_eq!(res.status_code, Some(StatusCode::FOUND));

        let state_cookie = res
            .cookies()
            .get("beam_oidc_state")
            .expect("state cookie should be set")
            .value()
            .to_string();
        let location = res
            .headers()
            .get("Location")
            .expect("Location header should be set")
            .to_str()
            .unwrap()
            .to_string();
        (state_cookie, location)
    }

    /// Drives `GET /callback` presenting the given state cookie, and the
    /// mock IdP's freshly-minted state/code as query params.
    async fn do_callback(harness: &Harness, state_cookie: &str, callback_state: &str) -> Response {
        TestClient::get(format!(
            "http://0.0.0.0/callback?state={callback_state}&code=fake-code"
        ))
        .add_header("Cookie", format!("beam_oidc_state={state_cookie}"), true)
        .send(&harness.service)
        .await
    }

    // ─── GET /login ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn login_redirects_to_idp_and_sets_state_cookie() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let (state_cookie, location) = do_login(&harness, None).await;
        assert!(!state_cookie.is_empty());
        assert!(location.starts_with("https://fake-idp.test/"));
    }

    // ─── GET /callback ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn callback_happy_path_provisions_user_and_sets_session_cookie() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "newuser@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, Some("/library")).await;
        let begin = harness.oidc_client.last_begin().unwrap();

        let mut res = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(res.status_code, Some(StatusCode::FOUND));

        let location = res.headers().get("Location").unwrap().to_str().unwrap();
        assert_eq!(location, "http://localhost:5173/library");

        let session_cookie = res.cookies().get("beam_session");
        assert!(session_cookie.is_some(), "session cookie should be set");

        let user = harness
            .user_repo
            .find_by_email("newuser@example.com")
            .await
            .unwrap()
            .expect("user should be JIT-provisioned");
        assert_eq!(user.oidc_issuer.as_deref(), Some("https://dex.test"));
        assert_eq!(user.oidc_subject.as_deref(), Some("subj-1"));
        assert!(
            !user.is_admin,
            "newuser@example.com is not in the admin allowlist"
        );

        let _ = res.take_string().await;
    }

    #[tokio::test]
    async fn callback_second_login_reuses_existing_user() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "returning@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie, &begin.state).await;

        let (state_cookie2, _) = do_login(&harness, None).await;
        let begin2 = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie2, &begin2.state).await;

        let user = harness
            .user_repo
            .find_by_email("returning@example.com")
            .await
            .unwrap();
        // Only one user was ever created for this (issuer, subject).
        assert_eq!(
            harness
                .session_store
                .list_for_user(&user.unwrap().id.to_string())
                .await
                .unwrap()
                .len(),
            2,
            "two logins should mint two sessions for the same user"
        );
    }

    #[tokio::test]
    async fn callback_unverified_email_never_grants_admin() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "admin@beam.localhost",
            false,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie, &begin.state).await;

        let user = harness
            .user_repo
            .find_by_email("admin@beam.localhost")
            .await
            .unwrap()
            .expect("user should still be provisioned");
        assert!(
            !user.is_admin,
            "unverified email must never grant admin, even if allowlisted"
        );
    }

    #[tokio::test]
    async fn callback_state_mismatch_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        // Present a state that doesn't match the cookie.
        let res = do_callback(&harness, &state_cookie, "wrong-state").await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn callback_cannot_be_replayed() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();

        let first = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(first.status_code, Some(StatusCode::FOUND));

        // Same state/cookie presented again -- already consumed.
        let second = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(second.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn callback_missing_state_cookie_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let res = TestClient::get("http://0.0.0.0/callback?state=whatever&code=fake-code")
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn callback_idp_error_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let res = TestClient::get(format!(
            "http://0.0.0.0/callback?state={state_cookie}&error=access_denied"
        ))
        .add_header("Cookie", format!("beam_oidc_state={state_cookie}"), true)
        .send(&harness.service)
        .await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn callback_exchange_failure_returns_400() {
        let harness = make_harness(FakeOidcClient::with_exchange_error("idp is down"));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let res = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    }

    // ─── GET /me, POST /logout, GET /sessions, DELETE /sessions/:id ──────────

    async fn login_and_get_session_cookie(harness: &Harness, email: &str) -> String {
        let (state_cookie, _) = do_login(harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let res = do_callback(harness, &state_cookie, &begin.state).await;
        res.cookies()
            .get("beam_session")
            .unwrap_or_else(|| panic!("expected a session cookie after login as {email}"))
            .value()
            .to_string()
    }

    #[tokio::test]
    async fn me_returns_current_user_with_valid_session_cookie() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "me@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "me@example.com").await;

        let mut res = TestClient::get("http://0.0.0.0/me")
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));

        let body: Value = res.take_json().await.unwrap();
        assert_eq!(body["email"], "me@example.com");
    }

    #[tokio::test]
    async fn me_without_session_cookie_returns_401() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "me@example.com",
            true,
        )));

        let res = TestClient::get("http://0.0.0.0/me")
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn logout_deletes_session_so_me_then_fails() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "logout@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "logout@example.com").await;

        let res = TestClient::post("http://0.0.0.0/logout")
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

        let res = TestClient::get("http://0.0.0.0/me")
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn list_and_delete_session_round_trip() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "sessions@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "sessions@example.com").await;

        let mut res = TestClient::get("http://0.0.0.0/sessions")
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::OK));
        let sessions: Vec<Value> = res.take_json().await.unwrap();
        assert_eq!(sessions.len(), 1);
        let session_id = sessions[0]["session_id"].as_str().unwrap().to_string();

        let res = TestClient::delete(format!("http://0.0.0.0/sessions/{session_id}"))
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

        // The session used to make this very request was just revoked.
        let res = TestClient::get("http://0.0.0.0/me")
            .add_header("Cookie", format!("beam_session={session_cookie}"), true)
            .send(&harness.service)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn delete_unknown_session_id_returns_401() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "sessions2@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "sessions2@example.com").await;

        let res =
            TestClient::delete("http://0.0.0.0/sessions/00000000-0000-0000-0000-000000000000")
                .add_header("Cookie", format!("beam_session={session_cookie}"), true)
                .send(&harness.service)
                .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }
}
