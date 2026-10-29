//! Subcutaneous tests for the OIDC BFF endpoints (ADR-0003).
//!
//! These drive the seven real handlers in [`crate::routes::auth`] through
//! Kynos's in-process `TestClient` over a real `AppState`, with a
//! `FakeOidcClient` and the in-memory stores below the trait line -- no IdP,
//! no Postgres, no listener (NFR-201).
//!
//! Two things moved with the framework and are worth knowing when reading
//! these. The routes now carry their production paths (`/v1/auth/login`,
//! `/v1/me`, ...) rather than the bare `/login`, `/me` the Salvo sub-router
//! mounted, because a Kynos route declares its own path. And the injected
//! dependencies arrive by *type* from the state, so the harness swaps them by
//! building an `AppServices` rather than by stacking `affix_state` hoops.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use beam_auth::utils::models::User;
    use beam_auth::utils::oidc::fake::FakeOidcClient;
    use beam_auth::utils::oidc::{OidcClient, OidcIdentity};
    use beam_auth::utils::oidc_config::OidcRuntimeConfig;
    use beam_auth::utils::pending_auth_store::PendingAuthStore;
    use beam_auth::utils::pending_auth_store::in_memory::InMemoryPendingAuthStore;
    use beam_auth::utils::repository::UserRepository;
    use beam_auth::utils::repository::in_memory::InMemoryUserRepository;
    use beam_auth::utils::session_store::SessionStore;
    use beam_auth::utils::session_store::in_memory::InMemorySessionStore;
    use kynos::http::StatusCode;
    use kynos::prelude::*;
    use kynos::test::{TestClient, TestResponse};
    use serde_json::{Value, json};

    use crate::routes::api_error::SESSION_COOKIE;
    use crate::routes::auth::{
        oidc_callback, oidc_delete_session, oidc_list_sessions, oidc_login, oidc_logout,
        oidc_logout_all, oidc_me,
    };
    use crate::routes::test_support::make_app_state;
    use crate::services::health::InMemoryDependencyProbe;
    use crate::state::{AppServices, AppState};

    const STATE_COOKIE: &str = "beam_oidc_state";

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
        client: TestClient<AppState>,
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
        make_harness_sharing_store(
            oidc_client,
            user_repo,
            Arc::new(InMemorySessionStore::default()),
            config,
        )
    }

    /// As [`make_harness_full`], but joins an existing session store.
    ///
    /// `FakeOidcClient` returns one fixed identity, so "two different users"
    /// means two routers with two clients -- which by default would also mean
    /// two isolated session stores, and then a cross-account revocation test
    /// would pass for the wrong reason.
    ///
    /// The auth dependencies are swapped by rebuilding [`AppServices`] on top
    /// of the shared stub state rather than by injecting them per route:
    /// `Inject<T>` resolves against the router's context, so the state *is* the
    /// injection point now. Every service this suite never touches is carried
    /// over from `test_support`'s stubs unchanged.
    fn make_harness_sharing_store(
        oidc_client: FakeOidcClient,
        user_repo: Arc<InMemoryUserRepository>,
        session_store: Arc<InMemorySessionStore>,
        config: OidcRuntimeConfig,
    ) -> Harness {
        make_harness_behind(
            oidc_client,
            user_repo,
            session_store,
            config,
            |_server_config| {},
        )
    }

    /// As [`make_harness_sharing_store`], with the router's trusted-proxy
    /// policy taken from a `ServerConfig` `adjust` writes -- through
    /// `ServerConfig::trusted_proxy_policy`, the mapping `main` applies.
    fn make_harness_behind(
        oidc_client: FakeOidcClient,
        user_repo: Arc<InMemoryUserRepository>,
        session_store: Arc<InMemorySessionStore>,
        config: OidcRuntimeConfig,
        adjust: impl FnOnce(&mut crate::config::ServerConfig),
    ) -> Harness {
        let oidc_client = Arc::new(oidc_client);
        let oidc_dyn: Arc<dyn OidcClient> = oidc_client.clone();
        let pending_auth_store: Arc<dyn PendingAuthStore> =
            Arc::new(InMemoryPendingAuthStore::default());
        let session_dyn: Arc<dyn SessionStore> = session_store.clone();
        let user_repo_dyn: Arc<dyn UserRepository> = user_repo.clone();

        let base = make_app_state();

        // The session TTLs live in two places: the handlers mint from
        // `OidcRuntimeConfig`, and `SessionAuthenticator` slides the idle
        // expiry from `ServerConfig`. A harness that let them disagree would
        // mint a session the very next request could not authenticate.
        let mut server_config = base.config.clone();
        server_config.web_url = config.web_url.clone();
        server_config.cookie_secure = Some(config.cookie_secure);
        server_config.session_idle_days = config.session_idle_days;
        server_config.session_max_days = config.session_max_days;
        adjust(&mut server_config);
        let trusted_proxies = server_config
            .trusted_proxy_policy()
            .expect("the test configuration names valid networks");

        let services = AppServices {
            hash: base.services.hash.clone(),
            access: base.services.access.clone(),
            library: base.services.library.clone(),
            metadata: base.services.metadata.clone(),
            subtitles: base.services.subtitles.clone(),
            notification: base.services.notification.clone(),
            admin_log: base.services.admin_log.clone(),
            user_repo: user_repo_dyn,
            language_preferences: crate::routes::test_support::idle_language_preferences(),
            playback: base.services.playback.clone(),
            genre_repo: base.services.genre_repo.clone(),
            library_repo: base.services.library_repo.clone(),
            file_repo: base.services.file_repo.clone(),
            enrichment_repo: base.services.enrichment_repo.clone(),
            enrichment_control: base.services.enrichment_control.clone(),
            people: base.services.people.clone(),
            lists: base.services.lists.clone(),
            collections: base.services.collections.clone(),
            movie_repo: base.services.movie_repo.clone(),
            show_repo: base.services.show_repo.clone(),
            artwork: base.services.artwork.clone(),
            session_store: session_dyn,
            oidc_client: oidc_dyn,
            pending_auth_store,
            device_auth_store: base.services.device_auth_store.clone(),
            oidc_config: config,
            watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
            telemetry: crate::routes::test_support::idle_library_report(),
            playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
            trickplay: crate::routes::test_support::idle_trickplay(),
        };

        let state = AppState::new(
            server_config,
            services,
            Arc::new(InMemoryDependencyProbe::healthy()),
            None,
        );

        let service = Router::new()
            .nest(
                "/v1",
                Router::new().mount(kynos::routes![
                    oidc_login,
                    oidc_callback,
                    oidc_me,
                    oidc_logout,
                    oidc_logout_all,
                    oidc_list_sessions,
                    oidc_delete_session,
                ]),
            )
            .trusted_proxies(trusted_proxies)
            .build(state)
            .expect("the auth router describes itself");

        Harness {
            client: TestClient::new(service),
            oidc_client,
            user_repo,
            session_store,
        }
    }

    /// The value of a live (non-clearing) cookie the response set, if any.
    ///
    /// A removal is emitted as `name=; ...; Max-Age=0`, which `cookies()`
    /// reports alongside a real one -- so "was a session issued?" is "is there
    /// a `beam_session` with a value?", not merely "is the name present?".
    fn live_cookie<'a>(response: &'a TestResponse, name: &str) -> Option<&'a str> {
        response
            .cookies()
            .into_iter()
            .find(|(set, value)| *set == name && !value.is_empty())
            .map(|(_, value)| value)
    }

    /// Drives `GET /v1/auth/login` and returns the `beam_oidc_state` cookie
    /// value and the redirect `Location` header.
    async fn do_login(harness: &Harness, redirect: Option<&str>) -> (String, String) {
        let path = match redirect {
            Some(r) => format!("/v1/auth/login?redirect={r}"),
            None => "/v1/auth/login".to_string(),
        };
        let response = harness.client.get(&path).send().await;
        assert_eq!(response.status(), StatusCode::FOUND);

        let state_cookie = live_cookie(&response, STATE_COOKIE)
            .expect("state cookie should be set")
            .to_string();
        let location = response
            .header("location")
            .expect("Location header should be set")
            .to_string();
        (state_cookie, location)
    }

    /// Drives `GET /v1/auth/callback` presenting the given state cookie, and
    /// the mock IdP's freshly-minted state/code as query params.
    async fn do_callback(
        harness: &Harness,
        state_cookie: &str,
        callback_state: &str,
    ) -> TestResponse {
        harness
            .client
            .get(&format!(
                "/v1/auth/callback?state={callback_state}&code=fake-code"
            ))
            .cookie(STATE_COOKIE, state_cookie)
            .send()
            .await
    }

    // ─── GET /v1/auth/login ────────────────────────────────────────────────

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

    /// The attributes of the live `name` cookie the response set, lowercased
    /// and without the `name=value` pair, so the set is comparable whatever
    /// order they are rendered in.
    fn cookie_attributes(response: &TestResponse, name: &str) -> BTreeSet<String> {
        let field = response
            .headers("set-cookie")
            .into_iter()
            .find(|field| field.starts_with(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{name} should be set"));
        field
            .split(';')
            .skip(1)
            .map(|attribute| attribute.trim().to_ascii_lowercase())
            .collect()
    }

    /// The cookie attributes are the session's security boundary: without
    /// `HttpOnly` a script can read the session, without `SameSite=Lax` a
    /// cross-site form can ride it, and without `Secure` an HTTPS deployment
    /// leaks it over plain HTTP. The state cookie is scoped to `/v1/auth` and
    /// lives only as long as the pending attempt; the session cookie lives as
    /// long as the session's absolute lifetime.
    #[tokio::test]
    async fn the_session_and_state_cookies_are_http_only_lax_scoped_and_secure_when_configured() {
        for secure in [false, true] {
            let config = OidcRuntimeConfig {
                cookie_secure: secure,
                ..test_config()
            };
            let session_max_age = config.absolute_ttl_secs();
            let harness = make_harness_full(
                FakeOidcClient::with_identity(identity("user@example.com", true)),
                Arc::new(InMemoryUserRepository::default()),
                config,
            );

            let expected = |path: &str, max_age: u64| {
                let mut attributes = BTreeSet::from([
                    "httponly".to_owned(),
                    "samesite=lax".to_owned(),
                    format!("path={path}"),
                    format!("max-age={max_age}"),
                ]);
                if secure {
                    attributes.insert("secure".to_owned());
                }
                attributes
            };

            let login = harness.client.get("/v1/auth/login").send().await;
            login.assert_status(StatusCode::FOUND);
            assert_eq!(
                cookie_attributes(&login, STATE_COOKIE),
                expected("/v1/auth", super::super::STATE_TTL_SECS),
                "state cookie, cookie_secure={secure}"
            );

            let state_cookie = live_cookie(&login, STATE_COOKIE).unwrap().to_owned();
            let begin = harness.oidc_client.last_begin().unwrap();
            let callback = do_callback(&harness, &state_cookie, &begin.state).await;
            callback.assert_status(StatusCode::FOUND);
            assert_eq!(
                cookie_attributes(&callback, SESSION_COOKIE),
                expected("/", session_max_age),
                "session cookie, cookie_secure={secure}"
            );
        }
    }

    /// The open-redirect guard end to end: a hostile `redirect` on login is
    /// stored as `/`, so the callback sends the browser to the web root rather
    /// than off-site. Each is a form some browser reads as another host.
    #[tokio::test]
    async fn a_hostile_redirect_lands_on_the_web_root_after_callback() {
        // URL-encoded: `//evil.example/x`, `https://evil.example/`, and
        // `/\evil.example`.
        for hostile in [
            "%2F%2Fevil.example%2Fx",
            "https%3A%2F%2Fevil.example%2F",
            "%2F%5Cevil.example",
        ] {
            let harness = make_harness(FakeOidcClient::with_identity(identity(
                "user@example.com",
                true,
            )));

            let (state_cookie, _) = do_login(&harness, Some(hostile)).await;
            let begin = harness.oidc_client.last_begin().unwrap();

            do_callback(&harness, &state_cookie, &begin.state)
                .await
                .assert_status(StatusCode::FOUND)
                .assert_redirect("http://localhost:5173/");
        }
    }

    /// With no identity provider configured, login is a 503 -- the server is
    /// up, sign-in is not -- and no login attempt is started. Driven through
    /// the served router, whose default state carries the
    /// `NotConfiguredOidcClient` a deployment without `BEAM_OIDC_*` gets.
    #[tokio::test]
    async fn login_with_an_unconfigured_idp_is_503() {
        let service = crate::routes::create_router(&crate::bootstrap::AllowedOrigins::none())
            .build(make_app_state())
            .expect("the served router describes itself");

        let response = TestClient::new(service).get("/v1/auth/login").send().await;

        response
            .assert_status(StatusCode::SERVICE_UNAVAILABLE)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#oidc-unavailable");
        assert_eq!(
            live_cookie(&response, STATE_COOKIE),
            None,
            "no attempt was started, so there is no state to bind"
        );
    }

    // ─── GET /v1/auth/callback ─────────────────────────────────────────────

    #[tokio::test]
    async fn callback_happy_path_provisions_user_and_sets_session_cookie() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "newuser@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, Some("/library")).await;
        let begin = harness.oidc_client.last_begin().unwrap();

        let response = do_callback(&harness, &state_cookie, &begin.state).await;
        response
            .assert_status(StatusCode::FOUND)
            .assert_redirect("http://localhost:5173/library");

        assert!(
            live_cookie(&response, SESSION_COOKIE).is_some(),
            "session cookie should be set"
        );

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
        let response = do_callback(&harness, &state_cookie, &begin.state).await;

        // Login still succeeds and mints a session.
        assert_eq!(response.status(), StatusCode::FOUND);
        assert!(
            live_cookie(&response, SESSION_COOKIE).is_some(),
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
        assert_eq!(res_a.status(), StatusCode::FOUND);

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
        assert_eq!(res_b.status(), StatusCode::FOUND);

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
        let response = do_callback(&harness, &state_cookie, "wrong-state").await;
        // The failure is an RFC 9457 problem document now, and the `type` is
        // the stable half of it -- the message never was contractual.
        response
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#login-attempt-invalid",
            );
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
        assert_eq!(first.status(), StatusCode::FOUND);

        // Same state/cookie presented again -- already consumed.
        let second = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_missing_state_cookie_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let response = harness
            .client
            .get("/v1/auth/callback?state=whatever&code=fake-code")
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_idp_error_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "user@example.com",
            true,
        )));

        let (state_cookie, _) = do_login(&harness, None).await;
        let response = harness
            .client
            .get(&format!(
                "/v1/auth/callback?state={state_cookie}&error=access_denied"
            ))
            .cookie(STATE_COOKIE, &state_cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_exchange_failure_returns_400() {
        let harness = make_harness(FakeOidcClient::with_exchange_error("idp is down"));

        let (state_cookie, _) = do_login(&harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let response = do_callback(&harness, &state_cookie, &begin.state).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        assert_eq!(ok.status(), StatusCode::FOUND);

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
        let response = do_callback(&harness, &state_cookie2, &begin2.state).await;
        response
            .assert_status(StatusCode::FORBIDDEN)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#account-disabled");
        assert!(
            live_cookie(&response, SESSION_COOKIE).is_none(),
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
        assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

        repo.set_disabled(user.id, false).await.unwrap();
        let (sc3, _) = do_login(&harness, None).await;
        let b3 = harness.oidc_client.last_begin().unwrap();
        let allowed = do_callback(&harness, &sc3, &b3.state).await;
        assert_eq!(allowed.status(), StatusCode::FOUND);
        assert!(
            live_cookie(&allowed, SESSION_COOKIE).is_some(),
            "a re-enabled account should receive a session cookie again"
        );
    }

    // ─── GET /v1/me, POST /v1/logout, /v1/sessions, /v1/sessions/{id} ───────

    /// Whether the response tells the browser to drop the session cookie.
    ///
    /// A clearing cookie is emitted as `beam_session=; ...; Max-Age=0` rather
    /// than a live one, so this reads the raw field -- which is what a browser
    /// acts on and therefore what this asserts.
    fn clears_session_cookie(response: &TestResponse) -> bool {
        response
            .headers("set-cookie")
            .iter()
            .any(|c| c.starts_with("beam_session=;"))
    }

    async fn login_and_get_session_cookie(harness: &Harness, email: &str) -> String {
        let (state_cookie, _) = do_login(harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let response = do_callback(harness, &state_cookie, &begin.state).await;
        live_cookie(&response, SESSION_COOKIE)
            .unwrap_or_else(|| panic!("expected a session cookie after login as {email}"))
            .to_string()
    }

    /// `GET /v1/me` as the holder of `session_cookie`.
    async fn get_me(harness: &Harness, session_cookie: &str) -> TestResponse {
        harness
            .client
            .get("/v1/me")
            .cookie(SESSION_COOKIE, session_cookie)
            .send()
            .await
    }

    #[tokio::test]
    async fn me_returns_current_user_with_valid_session_cookie() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "me@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "me@example.com").await;

        let response = get_me(&harness, &session_cookie).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body: Value = response.json();
        assert_eq!(body["email"], "me@example.com");
    }

    #[tokio::test]
    async fn me_without_session_cookie_returns_401() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "me@example.com",
            true,
        )));

        let response = harness.client.get("/v1/me").send().await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_deletes_session_so_me_then_fails() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "logout@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "logout@example.com").await;

        let response = harness
            .client
            .post("/v1/logout")
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            clears_session_cookie(&response),
            "logging out must also clear the cookie"
        );

        assert_eq!(
            get_me(&harness, &session_cookie).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// The session ids `GET /v1/sessions` reports for the holder of `cookie`.
    async fn list_session_ids(harness: &Harness, cookie: &str) -> Vec<String> {
        let response = harness
            .client
            .get("/v1/sessions")
            .cookie(SESSION_COOKIE, cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.json::<Value>();
        assert_eq!(
            body["page_info"]["has_next_page"], false,
            "the whole list is one page"
        );
        body["items"]
            .as_array()
            .expect("a connection's items")
            .iter()
            .map(|session| session["id"].as_str().expect("a session id").to_string())
            .collect()
    }

    #[tokio::test]
    async fn list_and_delete_session_round_trip() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "sessions@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "sessions@example.com").await;

        let ids = list_session_ids(&harness, &session_cookie).await;
        assert_eq!(ids.len(), 1);

        let response = harness
            .client
            .delete(&format!("/v1/sessions/{}", ids[0]))
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The session used to make this very request was just revoked.
        assert_eq!(
            get_me(&harness, &session_cookie).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// An id naming no session is `404 #session-not-found`, and the caller
    /// stays signed in: it is the id that failed, not their credential.
    #[tokio::test]
    async fn delete_unknown_session_id_returns_404() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "sessions2@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "sessions2@example.com").await;

        harness
            .client
            .delete("/v1/sessions/00000000-0000-0000-0000-000000000000")
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#session-not-found",
            );
        assert_eq!(
            get_me(&harness, &session_cookie).await.status(),
            StatusCode::OK
        );
    }

    /// A well-formed id nobody owns is a 404; a malformed one is a 400.
    ///
    /// It used to be a 500, because the handler flattened every
    /// `SessionError` into `Internal`. The path id is typed `Uuid` now, so the
    /// `Path` extractor answers the 400 before the store is reached at all.
    #[tokio::test]
    async fn delete_malformed_session_id_returns_400() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "sessions3@example.com",
            true,
        )));
        let session_cookie = login_and_get_session_cookie(&harness, "sessions3@example.com").await;

        harness
            .client
            .delete("/v1/sessions/not-a-uuid")
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    /// The store keeps epoch seconds; the list reports them as the instants
    /// they are. Sign-in and last use are two hours apart, so a swapped or
    /// mis-scaled conversion cannot pass.
    #[tokio::test]
    async fn listed_sessions_report_when_they_were_signed_in_and_last_used() {
        let signed_in_at =
            chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a representable instant");
        let clock = Arc::new(beam_domain::services::TestClock::starting_at(signed_in_at));
        let harness = make_harness_sharing_store(
            FakeOidcClient::with_identity(identity("instants@example.com", true)),
            Arc::new(InMemoryUserRepository::default()),
            Arc::new(InMemorySessionStore::new(clock.clone())),
            test_config(),
        );
        let session_cookie = login_and_get_session_cookie(&harness, "instants@example.com").await;

        // Past the touch throttle, so this request slides `last_active`.
        clock.advance(std::time::Duration::from_secs(2 * 3600));
        assert_eq!(
            get_me(&harness, &session_cookie).await.status(),
            StatusCode::OK
        );

        let response = harness
            .client
            .get("/v1/sessions")
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.json::<Value>();
        let sessions = body["items"].as_array().expect("a connection's items");
        assert_eq!(sessions.len(), 1);
        let instant = |field: &str| {
            sessions[0][field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} is a date-time string"))
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap_or_else(|_| panic!("{field} parses as RFC 3339"))
        };
        assert_eq!(instant("created_at"), signed_in_at);
        assert_eq!(
            instant("last_active_at"),
            signed_in_at + chrono::Duration::hours(2)
        );
    }

    /// Signs in through a callback that arrives from the socket `peer`
    /// carrying every forwarding header a client could forge, and returns the
    /// address `GET /v1/sessions` then reports for that session.
    async fn session_ip_after_callback_from(harness: &Harness, peer: &str) -> String {
        let (state_cookie, _) = do_login(harness, None).await;
        let begin = harness.oidc_client.last_begin().unwrap();
        let response = harness
            .client
            .get(&format!(
                "/v1/auth/callback?state={}&code=fake-code",
                begin.state
            ))
            .cookie(STATE_COOKIE, &state_cookie)
            .peer(peer.parse().expect("a socket address"))
            .header("X-Forwarded-For", "198.51.100.4")
            .header("X-Real-IP", "192.0.2.99")
            .header("User-Agent", "BeamTV/1.0")
            .send()
            .await;
        response.assert_status(StatusCode::FOUND);
        let session_cookie = live_cookie(&response, SESSION_COOKIE)
            .expect("a session was minted")
            .to_owned();

        let listed = harness
            .client
            .get("/v1/sessions")
            .cookie(SESSION_COOKIE, &session_cookie)
            .send()
            .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let body = listed.json::<Value>();
        let sessions = body["items"].as_array().expect("a connection's items");
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0]["device_hash"],
            beam_auth::utils::login::device_hash(Some("BeamTV/1.0")),
            "the User-Agent still reaches the session's device fingerprint"
        );
        sessions[0]["ip"]
            .as_str()
            .expect("the session's address is a string")
            .to_owned()
    }

    /// Issue #200: with no proxy trusted -- the default -- the session records
    /// the socket peer, and neither `X-Forwarded-For` nor `X-Real-IP` the
    /// client wrote changes it.
    #[tokio::test]
    async fn a_session_records_the_socket_peer_when_no_proxy_is_trusted() {
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "peer@example.com",
            true,
        )));
        assert_eq!(
            session_ip_after_callback_from(&harness, "203.0.113.7:41000").await,
            "203.0.113.7"
        );
    }

    /// Behind a proxy listed in `BEAM_TRUSTED_PROXIES`, the session records the
    /// client that proxy appended to `X-Forwarded-For`; a peer outside the
    /// list is still recorded as itself.
    #[tokio::test]
    async fn a_session_records_the_client_a_trusted_proxy_names() {
        let behind_proxy = || {
            make_harness_behind(
                FakeOidcClient::with_identity(identity("proxied@example.com", true)),
                Arc::new(InMemoryUserRepository::default()),
                Arc::new(InMemorySessionStore::default()),
                test_config(),
                |server_config| server_config.trusted_proxies = vec!["10.0.0.0/8".to_owned()],
            )
        };

        assert_eq!(
            session_ip_after_callback_from(&behind_proxy(), "10.1.2.3:443").await,
            "198.51.100.4",
            "the listed proxy names the client"
        );
        assert_eq!(
            session_ip_after_callback_from(&behind_proxy(), "203.0.113.7:41000").await,
            "203.0.113.7",
            "a peer outside the list cannot name its own address"
        );
    }

    /// A stored second no `DateTime` can hold is a corrupt row: reported as
    /// an error rather than rendered as some other instant. The in-memory
    /// store stamps from a `DateTime` clock, so only the SQL store can hold
    /// such a row, and the conversion is exercised directly.
    #[test]
    fn a_session_with_an_unrepresentable_timestamp_is_an_error() {
        use crate::routes::auth::SessionSummary;
        use beam_auth::utils::session_store::SessionData;

        let id = uuid::Uuid::nil();
        let data = |created_at: i64, last_active: i64| SessionData {
            user_id: "00000000-0000-0000-0000-000000000001".to_string(),
            device_hash: "device".to_string(),
            ip: "127.0.0.1".to_string(),
            created_at,
            last_active,
        };

        assert!(SessionSummary::from_store(id, data(i64::MAX, 0)).is_err());
        assert!(SessionSummary::from_store(id, data(0, i64::MAX)).is_err());
        let summary = SessionSummary::from_store(id, data(1_700_000_000, 1_700_007_200))
            .expect("representable seconds convert");
        assert_eq!(summary.created_at.timestamp(), 1_700_000_000);
        assert_eq!(summary.last_active_at.timestamp(), 1_700_007_200);
    }

    #[tokio::test]
    async fn logging_out_everywhere_revokes_every_session_of_that_user_only() {
        // The mutant this pins: replacing the handler body with `Ok(())`
        // returns 204 and leaves every session alive. Asserting the status
        // alone cannot tell the difference.
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "everywhere@example.com",
            true,
        )));
        let first = login_and_get_session_cookie(&harness, "everywhere@example.com").await;
        let second = login_and_get_session_cookie(&harness, "everywhere@example.com").await;
        assert_ne!(first, second, "two logins are two sessions");

        // A genuinely different user over the *same* store, whose session must
        // survive. Identity is `(issuer, subject)`, so the subject has to
        // differ -- `identity()` pins it, which would make this one account.
        let bystander_harness = make_harness_sharing_store(
            FakeOidcClient::with_identity(identity_with(
                "https://dex.test",
                "subj-bystander",
                Some("bystander@example.com"),
                Some("Bystander"),
            )),
            harness.user_repo.clone(),
            harness.session_store.clone(),
            test_config(),
        );
        let bystander_cookie =
            login_and_get_session_cookie(&bystander_harness, "bystander@example.com").await;

        let response = harness
            .client
            .post("/v1/logout-all")
            .cookie(SESSION_COOKIE, &first)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        for cookie in [&first, &second] {
            assert_eq!(
                get_me(&harness, cookie).await.status(),
                StatusCode::UNAUTHORIZED,
                "every session of the caller must be gone"
            );
        }
        assert_eq!(
            get_me(&bystander_harness, &bystander_cookie).await.status(),
            StatusCode::OK,
            "logging one user out everywhere must not touch anyone else"
        );
    }

    #[tokio::test]
    async fn one_user_cannot_revoke_another_users_session_by_its_id() {
        // The ownership check in `oidc_delete_session` is the only thing
        // standing between a listed session id and a cross-account logout.
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let session_store = Arc::new(InMemorySessionStore::default());

        // Distinct subjects: identity is `(issuer, subject)`, and `identity()`
        // pins the subject, so two calls to it are one account.
        let victim = make_harness_sharing_store(
            FakeOidcClient::with_identity(identity_with(
                "https://dex.test",
                "subj-victim",
                Some("victim@example.com"),
                Some("Victim"),
            )),
            user_repo.clone(),
            session_store.clone(),
            test_config(),
        );
        let attacker = make_harness_sharing_store(
            FakeOidcClient::with_identity(identity_with(
                "https://dex.test",
                "subj-attacker",
                Some("attacker@example.com"),
                Some("Attacker"),
            )),
            user_repo.clone(),
            session_store.clone(),
            test_config(),
        );

        let victim_cookie = login_and_get_session_cookie(&victim, "victim@example.com").await;
        let attacker_cookie = login_and_get_session_cookie(&attacker, "attacker@example.com").await;
        assert_ne!(victim_cookie, attacker_cookie);

        let victim_sessions = list_session_ids(&victim, &victim_cookie).await;
        assert_eq!(
            victim_sessions.len(),
            1,
            "the victim sees only their own session"
        );

        let response = attacker
            .client
            .delete(&format!("/v1/sessions/{}", victim_sessions[0]))
            .cookie(SESSION_COOKIE, &attacker_cookie)
            .send()
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "another user's session id must not be revocable, and must not be \
             distinguishable from one that does not exist"
        );

        assert_eq!(
            get_me(&victim, &victim_cookie).await.status(),
            StatusCode::OK,
            "the victim is still signed in"
        );
    }

    #[tokio::test]
    async fn revoking_a_session_other_than_the_current_one_leaves_the_cookie_alone() {
        // Whether the caller's own cookie is cleared is decided by re-reading
        // their token after the revocation. Inverting that would sign the
        // caller out every time they revoked one of their *other* devices.
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "twodevices@example.com",
            true,
        )));

        // Log in the old device first and read its id while it is the only
        // session, so the id is known rather than guessed at.
        let old_device = login_and_get_session_cookie(&harness, "twodevices@example.com").await;
        let ids = list_session_ids(&harness, &old_device).await;
        assert_eq!(ids.len(), 1);
        let old_device_id = ids[0].clone();

        let current = login_and_get_session_cookie(&harness, "twodevices@example.com").await;
        assert_ne!(old_device, current);

        let response = harness
            .client
            .delete(&format!("/v1/sessions/{old_device_id}"))
            .cookie(SESSION_COOKIE, &current)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            !clears_session_cookie(&response),
            "revoking another device must not clear the caller's own cookie"
        );

        // The caller is still signed in; the other device is not.
        assert_eq!(
            get_me(&harness, &current).await.status(),
            StatusCode::OK,
            "the caller keeps the session they were holding"
        );
        assert_eq!(
            get_me(&harness, &old_device).await.status(),
            StatusCode::UNAUTHORIZED,
            "the revoked device is signed out"
        );
    }

    #[tokio::test]
    async fn revoking_the_session_in_use_clears_the_callers_cookie() {
        // The other half of the same condition: when the id *is* the caller's
        // own session, leaving a live cookie behind for a dead session makes
        // every later request a confusing 401.
        let harness = make_harness(FakeOidcClient::with_identity(identity(
            "selfrevoke@example.com",
            true,
        )));
        let cookie = login_and_get_session_cookie(&harness, "selfrevoke@example.com").await;

        let ids = list_session_ids(&harness, &cookie).await;
        let own_id = ids[0].clone();

        let response = harness
            .client
            .delete(&format!("/v1/sessions/{own_id}"))
            .cookie(SESSION_COOKIE, &cookie)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            clears_session_cookie(&response),
            "revoking the session in use must clear its cookie"
        );
    }

    /// An identity with an explicit name and picture, for the profile-refresh
    /// tests. `identity_with` deliberately leaves `picture` unset.
    fn identity_with_profile(email: &str, name: &str, picture: &str) -> OidcIdentity {
        OidcIdentity {
            issuer: "https://idp.test".to_string(),
            subject: format!("sub-{email}"),
            email: Some(email.to_string()),
            email_verified: true,
            name: Some(name.to_string()),
            picture: Some(picture.to_string()),
            claims: json!({}),
        }
    }

    #[tokio::test]
    async fn a_second_login_with_an_unchanged_profile_does_not_rewrite_it() {
        // `display_name != .. || avatar_url != ..` gates the profile refresh.
        // Inverting either comparison writes on every login (or never writes),
        // and the response body looks identical either way -- so this asserts
        // the stored row instead.
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let steady = identity_with_profile(
            "steady@example.com",
            "Steady Eddie",
            "https://idp.test/steady.png",
        );

        let first = make_harness_full(
            FakeOidcClient::with_identity(steady.clone()),
            user_repo.clone(),
            test_config(),
        );
        let _ = login_and_get_session_cookie(&first, "steady@example.com").await;
        let user = user_repo
            .find_by_oidc_identity("https://idp.test", "sub-steady@example.com")
            .await
            .unwrap()
            .expect("provisioned on first login");
        assert_eq!(user.display_name, "Steady Eddie");
        assert_eq!(
            user.avatar_url.as_deref(),
            Some("https://idp.test/steady.png")
        );

        let second = make_harness_full(
            FakeOidcClient::with_identity(steady),
            user_repo.clone(),
            test_config(),
        );
        let _ = login_and_get_session_cookie(&second, "steady@example.com").await;

        let after = user_repo
            .find_by_oidc_identity("https://idp.test", "sub-steady@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.id, user.id, "the same account, not a second one");
        assert_eq!(after.display_name, "Steady Eddie");
        assert_eq!(
            after.avatar_url.as_deref(),
            Some("https://idp.test/steady.png")
        );
    }

    /// Log in once with `before`, then again with `after`, over one user
    /// repository, and return the stored row afterwards.
    async fn relogin_with(subject: &str, before: OidcIdentity, after: OidcIdentity) -> User {
        let user_repo = Arc::new(InMemoryUserRepository::default());
        let first = make_harness_full(
            FakeOidcClient::with_identity(before),
            user_repo.clone(),
            test_config(),
        );
        let _ = login_and_get_session_cookie(&first, subject).await;

        let second = make_harness_full(
            FakeOidcClient::with_identity(after),
            user_repo.clone(),
            test_config(),
        );
        let _ = login_and_get_session_cookie(&second, subject).await;

        user_repo
            .find_by_oidc_identity("https://idp.test", subject)
            .await
            .unwrap()
            .expect("provisioned on the first login")
    }

    #[tokio::test]
    async fn a_changed_name_alone_is_written_through() {
        // The refresh condition is `name != .. || picture != ..`. With `&&`
        // instead, a change to only one field is silently dropped -- and a
        // display name is the field users actually change.
        let stored = relogin_with(
            "sub-nameonly@example.com",
            identity_with_profile(
                "nameonly@example.com",
                "Old Name",
                "https://idp.test/same.png",
            ),
            identity_with_profile(
                "nameonly@example.com",
                "New Name",
                "https://idp.test/same.png",
            ),
        )
        .await;

        assert_eq!(stored.display_name, "New Name");
        assert_eq!(
            stored.avatar_url.as_deref(),
            Some("https://idp.test/same.png")
        );
    }

    #[tokio::test]
    async fn a_changed_picture_alone_is_written_through() {
        let stored = relogin_with(
            "sub-piconly@example.com",
            identity_with_profile(
                "piconly@example.com",
                "Same Name",
                "https://idp.test/old.png",
            ),
            identity_with_profile(
                "piconly@example.com",
                "Same Name",
                "https://idp.test/new.png",
            ),
        )
        .await;

        assert_eq!(stored.display_name, "Same Name");
        assert_eq!(
            stored.avatar_url.as_deref(),
            Some("https://idp.test/new.png")
        );
    }

    #[tokio::test]
    async fn a_changed_name_or_picture_at_the_idp_is_written_through_on_the_next_login() {
        let stored = relogin_with(
            "sub-changing@example.com",
            identity_with_profile(
                "changing@example.com",
                "Old Name",
                "https://idp.test/old.png",
            ),
            identity_with_profile(
                "changing@example.com",
                "New Name",
                "https://idp.test/new.png",
            ),
        )
        .await;

        assert_eq!(stored.display_name, "New Name");
        assert_eq!(
            stored.avatar_url.as_deref(),
            Some("https://idp.test/new.png")
        );
    }
}
