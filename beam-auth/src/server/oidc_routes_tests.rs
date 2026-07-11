#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use salvo::prelude::*;
    use salvo::test::{ResponseExt, TestClient};
    use serde_json::{Value, json};

    use crate::server::oidc_routes::{OidcRuntimeConfig, oidc_routes};
    use crate::utils::oidc::fake::FakeOidcClient;
    use crate::utils::oidc::{OidcClient, OidcIdentity};
    use crate::utils::pending_auth_store::PendingAuthStore;
    use crate::utils::pending_auth_store::in_memory::InMemoryPendingAuthStore;
    use crate::utils::repository::UserRepository;
    use crate::utils::repository::in_memory::InMemoryUserRepository;
    use crate::utils::session_store::SessionStore;
    use crate::utils::session_store::in_memory::InMemorySessionStore;

    /// Default runtime config for the harness: admin is bound to a `groups`
    /// claim containing `beam-admin`, mirroring a typical Dex/Keycloak setup.
    fn test_config() -> OidcRuntimeConfig {
        OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_string(),
            cookie_secure: false,
            admin_claim: Some("groups".to_string()),
            admin_value: Some("beam-admin".to_string()),
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
            // No admin-granting claim by default.
            claims: json!({}),
        }
    }

    /// A `(issuer, subject)` = `(https://dex.test, subj-1)` identity whose
    /// released claim set is fully caller-supplied, for admin-evaluation tests.
    fn identity_with_claims(claims: Value) -> OidcIdentity {
        OidcIdentity {
            claims,
            ..identity("user@example.com", true)
        }
    }

    /// Full control over the claims a fake IdP releases, for cases the terse
    /// `identity` helper can't express: a missing email claim, or a specific
    /// `(issuer, subject)` pair.
    fn identity_with(
        issuer: &str,
        subject: &str,
        email: Option<&str>,
        name: Option<&str>,
    ) -> OidcIdentity {
        OidcIdentity {
            issuer: issuer.to_string(),
            subject: subject.to_string(),
            email: email.map(str::to_string),
            email_verified: email.is_some(),
            name: name.map(str::to_string),
            picture: None,
            claims: json!({}),
        }
    }

    struct Harness {
        service: Service,
        oidc_client: Arc<FakeOidcClient>,
        user_repo: Arc<InMemoryUserRepository>,
        session_store: Arc<InMemorySessionStore>,
    }

    fn make_harness(oidc_client: FakeOidcClient) -> Harness {
        make_harness_with_repo(oidc_client, Arc::new(InMemoryUserRepository::default()))
    }

    /// Builds a harness around a caller-supplied user repository, so several
    /// harnesses (e.g. two different issuers) can provision into the *same*
    /// store and assertions can span both logins.
    fn make_harness_with_repo(
        oidc_client: FakeOidcClient,
        user_repo: Arc<InMemoryUserRepository>,
    ) -> Harness {
        make_harness_full(oidc_client, user_repo, test_config())
    }

    /// As [`make_harness_with_repo`], but with a caller-supplied runtime config
    /// -- for exercising different admin-claim configurations.
    fn make_harness_full(
        oidc_client: FakeOidcClient,
        user_repo: Arc<InMemoryUserRepository>,
        config: OidcRuntimeConfig,
    ) -> Harness {
        let oidc_client = Arc::new(oidc_client);
        let oidc_dyn: Arc<dyn OidcClient> = oidc_client.clone();
        let pending_auth_store: Arc<dyn PendingAuthStore> =
            Arc::new(InMemoryPendingAuthStore::default());
        let session_store = Arc::new(InMemorySessionStore::default());
        let session_dyn: Arc<dyn SessionStore> = session_store.clone();
        let user_repo_dyn: Arc<dyn UserRepository> = user_repo.clone();

        let router = Router::new()
            .hoop(affix_state::inject(oidc_dyn))
            .hoop(affix_state::inject(pending_auth_store))
            .hoop(affix_state::inject(session_dyn))
            .hoop(affix_state::inject(user_repo_dyn))
            .hoop(affix_state::inject(config))
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
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should be JIT-provisioned");
        assert_eq!(user.oidc_issuer, "https://dex.test");
        assert_eq!(user.oidc_subject, "subj-1");
        assert!(
            !user.is_admin,
            "newuser@example.com released no admin-granting claim"
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
            .find_by_oidc_identity("https://dex.test", "subj-1")
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
    async fn callback_without_email_claim_provisions_user_with_null_email() {
        // An IdP that releases no `email` claim must still provision a working
        // account -- the `users.email NOT NULL UNIQUE` leftover that issue #79
        // removes would have rejected this insert at the database.
        let harness = make_harness(FakeOidcClient::with_identity(identity_with(
            "https://dex.test",
            "subj-1",
            None,
            None,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let res = do_callback(&harness, &state_cookie, &begin.state).await;

        // Login still succeeds and mints a session.
        assert_eq!(res.status_code, Some(StatusCode::FOUND));
        assert!(
            res.cookies().get("beam_session").is_some(),
            "session cookie should be set even with no email claim"
        );

        let user = harness
            .user_repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should be JIT-provisioned without an email claim");
        assert_eq!(user.email, None, "email must be stored as NULL, not \"\"");
        assert_eq!(
            user.display_name, "user-subj-1",
            "display_name falls back to a subject-derived placeholder"
        );
        assert!(
            !user.is_admin,
            "an identity with no admin claim is never admin"
        );
    }

    #[tokio::test]
    async fn callback_same_email_under_different_issuers_both_provision() {
        // Email is non-unique per data-model.md: the same address can appear
        // under more than one issuer. Both logins must provision distinct
        // users into the shared store -- the dropped `users_email_key` unique
        // constraint (issue #79) would otherwise reject the second insert.
        let shared_repo = Arc::new(InMemoryUserRepository::default());

        let harness_a = make_harness_with_repo(
            FakeOidcClient::with_identity(identity_with(
                "https://issuer-a.test",
                "subj-a",
                Some("shared@example.com"),
                Some("User A"),
            )),
            shared_repo.clone(),
        );
        let (state_a, _) = do_login(&harness_a, None).await;
        let begin_a = harness_a.oidc_client.last_begin().unwrap();
        let res_a = do_callback(&harness_a, &state_a, &begin_a.state).await;
        assert_eq!(res_a.status_code, Some(StatusCode::FOUND));

        let harness_b = make_harness_with_repo(
            FakeOidcClient::with_identity(identity_with(
                "https://issuer-b.test",
                "subj-b",
                Some("shared@example.com"),
                Some("User B"),
            )),
            shared_repo.clone(),
        );
        let (state_b, _) = do_login(&harness_b, None).await;
        let begin_b = harness_b.oidc_client.last_begin().unwrap();
        let res_b = do_callback(&harness_b, &state_b, &begin_b.state).await;
        assert_eq!(res_b.status_code, Some(StatusCode::FOUND));

        let user_a = shared_repo
            .find_by_oidc_identity("https://issuer-a.test", "subj-a")
            .await
            .unwrap()
            .expect("issuer-a user should be provisioned");
        let user_b = shared_repo
            .find_by_oidc_identity("https://issuer-b.test", "subj-b")
            .await
            .unwrap()
            .expect("issuer-b user should be provisioned");

        assert_ne!(
            user_a.id, user_b.id,
            "distinct issuers yield distinct users"
        );
        assert_eq!(user_a.email.as_deref(), Some("shared@example.com"));
        assert_eq!(user_b.email.as_deref(), Some("shared@example.com"));
    }

    /// Drives a login for the default `(https://dex.test, subj-1)` identity
    /// with the given claim set, into the supplied shared repo, and returns the
    /// provisioned user's `is_admin`.
    async fn login_admin_status(user_repo: Arc<InMemoryUserRepository>, claims: Value) -> bool {
        let harness = make_harness_with_repo(
            FakeOidcClient::with_identity(identity_with_claims(claims)),
            user_repo.clone(),
        );
        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie, &begin.state).await;
        user_repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should be provisioned")
            .is_admin
    }

    #[tokio::test]
    async fn callback_matching_group_claim_grants_admin() {
        // Config binds admin to `groups` containing `beam-admin`.
        let is_admin = login_admin_status(
            Arc::new(InMemoryUserRepository::default()),
            json!({ "groups": ["users", "beam-admin"] }),
        )
        .await;
        assert!(is_admin, "an array claim containing the value grants admin");
    }

    #[tokio::test]
    async fn callback_absent_claim_does_not_grant_admin() {
        let is_admin = login_admin_status(
            Arc::new(InMemoryUserRepository::default()),
            json!({ "groups": ["users"] }),
        )
        .await;
        assert!(!is_admin, "the value is not in the released groups claim");
    }

    #[tokio::test]
    async fn callback_recompute_demotes_admin_when_claim_disappears() {
        // The IdP is the single authority: a user who was admin loses it at the
        // next login once the granting claim is gone (issue #85).
        let repo = Arc::new(InMemoryUserRepository::default());

        let granted = login_admin_status(repo.clone(), json!({ "groups": ["beam-admin"] })).await;
        assert!(granted, "first login with the claim grants admin");

        let after = login_admin_status(repo.clone(), json!({ "groups": ["users"] })).await;
        assert!(
            !after,
            "second login without the claim must demote the same user"
        );
    }

    #[tokio::test]
    async fn callback_recompute_promotes_user_when_claim_appears() {
        // The reverse: a non-admin gains admin once the IdP starts releasing
        // the claim, without any server-side toggle.
        let repo = Arc::new(InMemoryUserRepository::default());

        let before = login_admin_status(repo.clone(), json!({ "groups": ["users"] })).await;
        assert!(!before, "first login without the claim is not admin");

        let after = login_admin_status(repo.clone(), json!({ "groups": ["beam-admin"] })).await;
        assert!(after, "second login with the claim promotes the same user");
    }

    #[tokio::test]
    async fn callback_no_admin_claim_configured_never_grants_admin() {
        // With BEAM_OIDC_ADMIN_CLAIM unset, nobody is admin -- even an identity
        // that carries a plausible admin-looking claim.
        let config = OidcRuntimeConfig {
            admin_claim: None,
            admin_value: None,
            ..test_config()
        };
        let repo = Arc::new(InMemoryUserRepository::default());
        let harness = make_harness_full(
            FakeOidcClient::with_identity(identity_with_claims(
                json!({ "groups": ["beam-admin"], "is_admin": true }),
            )),
            repo.clone(),
            config,
        );

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie, &begin.state).await;

        let user = repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should still be provisioned");
        assert!(
            !user.is_admin,
            "no configured admin claim means nobody is admin"
        );
    }

    #[tokio::test]
    async fn callback_boolean_claim_grants_admin_when_no_value_expected() {
        // A config with ADMIN_CLAIM but no ADMIN_VALUE requires a boolean-true claim.
        let config = OidcRuntimeConfig {
            admin_claim: Some("is_admin".to_string()),
            admin_value: None,
            ..test_config()
        };
        let repo = Arc::new(InMemoryUserRepository::default());
        let harness = make_harness_full(
            FakeOidcClient::with_identity(identity_with_claims(json!({ "is_admin": true }))),
            repo.clone(),
            config,
        );

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &state_cookie, &begin.state).await;

        let user = repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should be provisioned");
        assert!(user.is_admin, "a boolean-true claim grants admin");
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

    #[tokio::test]
    async fn callback_disabled_user_is_rejected_without_a_session() {
        // A previously-provisioned account that has since been disabled must be
        // blocked at the callback: 403, no session cookie, and the session
        // store stays empty for that user (issue #85).
        let repo = Arc::new(InMemoryUserRepository::default());
        let harness = make_harness_with_repo(
            FakeOidcClient::with_identity(identity("blocked@example.com", true)),
            repo.clone(),
        );

        // First login provisions the account and mints a session.
        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let ok = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(ok.status_code, Some(StatusCode::FOUND));

        let user = repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .expect("user should be provisioned on first login");

        // An admin disables the account, revoking its live sessions.
        repo.set_disabled(user.id, true).await.unwrap();
        harness
            .session_store
            .delete_all_for_user(&user.id.to_string())
            .await
            .unwrap();

        // A fresh login attempt is refused with 403 and no new session.
        let (state_cookie2, _) = do_login(&harness, None).await;
        let begin2 = harness.oidc_client.last_begin().unwrap();
        let res = do_callback(&harness, &state_cookie2, &begin2.state).await;
        assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
        assert!(
            res.cookies().get("beam_session").is_none(),
            "a disabled account must not receive a session cookie"
        );
        assert_eq!(
            harness
                .session_store
                .list_for_user(&user.id.to_string())
                .await
                .unwrap()
                .len(),
            0,
            "no session should exist for a rejected disabled account"
        );
    }

    #[tokio::test]
    async fn callback_re_enabled_user_can_log_in_again() {
        // Re-enabling a disabled account restores login -- disabling is a
        // reversible moderation switch, not a permanent ban (issue #85).
        let repo = Arc::new(InMemoryUserRepository::default());
        let harness = make_harness_with_repo(
            FakeOidcClient::with_identity(identity("toggled@example.com", true)),
            repo.clone(),
        );

        let (sc1, _) = do_login(&harness, None).await;
        let b1 = harness.oidc_client.last_begin().unwrap();
        do_callback(&harness, &sc1, &b1.state).await;
        let user = repo
            .find_by_oidc_identity("https://dex.test", "subj-1")
            .await
            .unwrap()
            .unwrap();

        repo.set_disabled(user.id, true).await.unwrap();
        let (sc2, _) = do_login(&harness, None).await;
        let b2 = harness.oidc_client.last_begin().unwrap();
        let blocked = do_callback(&harness, &sc2, &b2.state).await;
        assert_eq!(blocked.status_code, Some(StatusCode::FORBIDDEN));

        repo.set_disabled(user.id, false).await.unwrap();
        let (sc3, _) = do_login(&harness, None).await;
        let b3 = harness.oidc_client.last_begin().unwrap();
        let allowed = do_callback(&harness, &sc3, &b3.state).await;
        assert_eq!(allowed.status_code, Some(StatusCode::FOUND));
        assert!(
            allowed.cookies().get("beam_session").is_some(),
            "a re-enabled account should receive a session cookie again"
        );
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
        let session_id = sessions[0]["id"].as_str().unwrap().to_string();

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
