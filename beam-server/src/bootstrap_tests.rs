use super::*;

fn https_deployment() -> ServerConfig {
    ServerConfig {
        server_url: "https://beam.example.com".to_string(),
        web_url: "https://beam.example.com".to_string(),
        ..Default::default()
    }
}

mod cookie_security_gate {
    use super::*;

    #[test]
    fn a_plain_http_dev_setup_starts_without_comment() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "http://localhost:5173".to_string(),
            ..Default::default()
        };
        assert_eq!(
            check_cookie_security(&config).unwrap(),
            StartupGate::Proceed
        );
    }

    #[test]
    fn a_fully_https_deployment_starts_without_comment() {
        assert_eq!(
            check_cookie_security(&https_deployment()).unwrap(),
            StartupGate::Proceed
        );
    }

    #[test]
    fn a_tls_terminating_proxy_without_an_override_refuses_to_start() {
        // The footgun this gate exists for: the server sees plain HTTP, the
        // browser sees HTTPS, and the session cookie would ship without
        // Secure on a production site.
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            ..Default::default()
        };

        let error = check_cookie_security(&config)
            .expect_err("an implicit insecure cookie on an HTTPS site must stop startup");
        assert!(
            error.contains("BEAM_SERVER_URL") && error.contains("BEAM_COOKIE_SECURE"),
            "the message must name both ways out: {error}"
        );
    }

    #[test]
    fn an_explicit_opt_out_starts_but_says_so() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            cookie_secure: Some(false),
            ..Default::default()
        };

        let StartupGate::ProceedWithWarning(warning) = check_cookie_security(&config).unwrap()
        else {
            panic!("an explicit opt-out must warn, not pass silently");
        };
        assert!(warning.contains("BEAM_COOKIE_SECURE=false"));
    }

    #[test]
    fn an_explicit_opt_in_resolves_the_misconfiguration() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            cookie_secure: Some(true),
            ..Default::default()
        };
        assert_eq!(
            check_cookie_security(&config).unwrap(),
            StartupGate::Proceed
        );
    }
}

mod retired_env_vars {
    use super::*;

    #[test]
    fn a_retired_variable_that_is_still_set_is_named_in_a_warning() {
        let warnings = retired_env_warnings(|name| name == "BEAM_HASH_UNKNOWN_FILES");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("BEAM_HASH_UNKNOWN_FILES "));
    }

    #[test]
    fn nothing_is_said_when_no_retired_variable_is_set() {
        assert!(retired_env_warnings(|_| false).is_empty());
    }
}

mod cors {
    use std::collections::BTreeSet;

    use kynos::http::StatusCode;
    use kynos::prelude::*;
    use kynos::response::status::NoContent;
    use kynos::test::TestClient;

    use super::*;

    // Three operations on one path, doing nothing: the policy is the subject,
    // and the *set* of methods is now part of what it answers with.
    #[kynos::get("/thing", operation_id = "corsProbe")]
    async fn probe() -> NoContent {
        NoContent
    }

    #[kynos::patch("/thing", operation_id = "corsProbePatch")]
    async fn probe_patch() -> NoContent {
        NoContent
    }

    #[kynos::delete("/thing", operation_id = "corsProbeDelete")]
    async fn probe_delete() -> NoContent {
        NoContent
    }

    /// `Cors` is an `Interceptor<C>` for any context, so the policy needs no
    /// `AppState` to be exercised. The allowed origin is the web one
    /// [`https_deployment`] configures.
    fn client() -> TestClient<()> {
        let service = Router::new()
            .mount(kynos::routes![probe, probe_patch, probe_delete])
            .intercept(cors_policy(&AllowedOrigins::from_config(
                &https_deployment(),
            )))
            .build(())
            .expect("the probe router describes itself");

        TestClient::new(service)
    }

    /// A preflight proposing `method`, from the configured web origin.
    async fn preflight(method: &str) -> kynos::test::TestResponse {
        client()
            .options("/thing")
            .header("origin", "https://beam.example.com")
            .header("access-control-request-method", method)
            .header("access-control-request-headers", "range")
            .send()
            .await
    }

    #[tokio::test]
    async fn a_preflight_allows_the_headers_the_player_and_client_send() {
        let response = preflight("GET").await;

        // Kynos answers a preflight with 204: it is a protocol answer rather
        // than an operation, so there is no body and no declared status.
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let allowed = response
            .header("access-control-allow-headers")
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            allowed.contains("range"),
            "without `range` the player cannot seek: {allowed}"
        );
        assert!(allowed.contains("content-type"), "{allowed}");
    }

    /// The advertised methods are derived from the operations declared on the
    /// matched path, so preflight and the description cannot disagree. The
    /// hand-written list this replaces named GET, POST, PUT, DELETE and
    /// OPTIONS -- but not PATCH, which `/v1/admin/users/{id}` has used since
    /// issue #85, so a browser preflighting that request was told no.
    ///
    /// HEAD is in the set because the path's GET answers it (RFC 9110
    /// section 9.3.2), which Kynos advertises from 0.4.0 on.
    #[tokio::test]
    async fn the_advertised_methods_are_the_ones_the_path_actually_declares() {
        let response = preflight("PATCH").await;

        let advertised: BTreeSet<&str> = response
            .header("access-control-allow-methods")
            .expect("a permitted preflight advertises the methods")
            .split(',')
            .map(str::trim)
            .collect();

        assert_eq!(
            advertised,
            BTreeSet::from(["GET", "HEAD", "PATCH", "DELETE"]),
            "the advertised set must be exactly what this path declares"
        );
    }

    #[tokio::test]
    async fn credentials_are_allowed_so_the_session_cookie_is_sent() {
        let response = client()
            .get("/thing")
            .header("origin", "https://beam.example.com")
            .send()
            .await;

        assert_eq!(
            response.header("access-control-allow-credentials"),
            Some("true"),
            "cookie-only auth needs credentialed CORS"
        );
    }

    #[tokio::test]
    async fn the_range_response_headers_are_exposed_to_the_browser() {
        // A browser cannot read `content-range` off a cross-origin response
        // unless it is explicitly exposed, and the player needs it to know how
        // much of the file it just received.
        let response = client()
            .get("/thing")
            .header("origin", "https://beam.example.com")
            .send()
            .await;

        let exposed = response
            .header("access-control-expose-headers")
            .unwrap_or_default()
            .to_ascii_lowercase();
        for header in ["accept-ranges", "content-length", "content-range"] {
            assert!(exposed.contains(header), "{header} not exposed: {exposed}");
        }
    }

    /// The origin is allow-listed, not mirrored: a credentialed response is
    /// readable only by an origin the configuration names (issue #348).
    #[tokio::test]
    async fn an_origin_the_configuration_does_not_name_gets_no_cors_headers() {
        let response = client()
            .get("/thing")
            .header("origin", "https://somewhere.else.example")
            .send()
            .await;

        assert_eq!(response.header("access-control-allow-origin"), None);
        assert_eq!(response.header("access-control-allow-credentials"), None);
    }
}

mod allowed_origins {
    use super::*;

    #[test]
    fn the_web_server_and_extra_origins_are_all_named_without_trailing_slashes() {
        let origins = AllowedOrigins::from_config(&ServerConfig {
            web_url: "https://app.example.com/".to_string(),
            server_url: "https://api.example.com".to_string(),
            extra_allowed_origins: Some(
                " https://one.example.com/ ,,https://two.example.com".to_string(),
            ),
            ..Default::default()
        });

        for origin in [
            "https://app.example.com",
            "https://api.example.com",
            "https://one.example.com",
            "https://two.example.com",
        ] {
            assert!(origins.contains(origin), "{origin} is configured");
        }
        assert!(
            !origins.contains(""),
            "an empty list entry is not an origin"
        );
        assert!(!origins.contains("https://app.example.com/"));
    }
}

/// The policy as `/v1` serves it: through `create_router`, over a real
/// `AppState`, with a session the stores' own traits minted. These are the
/// tests the mirrored policy would have failed -- the router is the subject,
/// so dropping or reordering the CORS interceptor is caught here.
mod served_router {
    use beam_auth::utils::models::CreateUser;
    use beam_auth::utils::session_store::SessionData;
    use kynos::http::StatusCode;
    use kynos::test::TestClient;

    use super::*;
    use crate::routes::create_router;
    use crate::routes::test_support::make_app_state_with;
    use crate::state::AppState;

    const WEB_ORIGIN: &str = "https://beam.example.com";
    const EXTRA_ORIGIN: &str = "https://admin.example.com";
    /// A same-site sibling: its requests carry a `SameSite=Lax` cookie, so
    /// only CORS stands between it and the response.
    const SIBLING_ORIGIN: &str = "https://evil.example.com";

    /// The served router over a state configured with [`WEB_ORIGIN`] and
    /// [`EXTRA_ORIGIN`], built with the origins `main` would pass.
    fn served() -> (TestClient<AppState>, AppState) {
        let state = make_app_state_with(|config| {
            config.web_url = WEB_ORIGIN.to_string();
            config.server_url = "https://api.example.com".to_string();
            config.extra_allowed_origins = Some(EXTRA_ORIGIN.to_string());
        });
        let service = create_router(&AllowedOrigins::from_config(&state.config))
            .build(state.clone())
            .expect("the served router describes itself");
        (TestClient::new(service), state)
    }

    /// Seeds a user and a session through the stores' own traits and returns
    /// the `beam_session` cookie value.
    async fn session(state: &AppState) -> String {
        let user = state
            .services
            .user_repo
            .create(CreateUser {
                oidc_issuer: "https://idp.example".to_string(),
                oidc_subject: "subject".to_string(),
                email: Some("someone@example.com".to_string()),
                display_name: "Someone".to_string(),
                avatar_url: None,
                is_admin: false,
            })
            .await
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        state
            .services
            .session_store
            .create(
                &SessionData {
                    user_id: user.id.to_string(),
                    device_hash: "test-device".to_string(),
                    ip: "127.0.0.1".to_string(),
                    created_at: now,
                    last_active: now,
                },
                86400,
                86400,
            )
            .await
            .unwrap()
    }

    async fn me_from(origin: &str) -> kynos::test::TestResponse {
        let (client, state) = served();
        let token = session(&state).await;
        client
            .get("/v1/me")
            .header("origin", origin)
            .cookie("beam_session", &token)
            .send()
            .await
    }

    #[tokio::test]
    async fn a_cross_origin_reader_cannot_read_an_authenticated_response() {
        let response = me_from(SIBLING_ORIGIN).await;

        // The server still answers -- CORS is enforced by the browser, which
        // withholds a response that carries no grant for the asking origin.
        response.assert_status(StatusCode::OK);
        assert_eq!(
            response.header("access-control-allow-origin"),
            None,
            "an origin nobody configured must not be granted a credentialed read"
        );
        assert_eq!(response.header("access-control-allow-credentials"), None);
    }

    #[tokio::test]
    async fn the_configured_web_origin_may_read_with_credentials() {
        for origin in [WEB_ORIGIN, EXTRA_ORIGIN] {
            let response = me_from(origin).await;

            response.assert_status(StatusCode::OK);
            assert_eq!(
                response.header("access-control-allow-origin"),
                Some(origin),
                "{origin} is configured"
            );
            assert_eq!(
                response.header("access-control-allow-credentials"),
                Some("true"),
                "{origin} is configured"
            );
        }
    }

    #[tokio::test]
    async fn a_preflight_from_an_unconfigured_origin_is_not_granted() {
        let (client, _) = served();
        let response = client
            .options("/v1/me")
            .header("origin", SIBLING_ORIGIN)
            .header("access-control-request-method", "GET")
            .send()
            .await;

        assert_eq!(response.header("access-control-allow-origin"), None);
    }

    #[tokio::test]
    async fn a_request_without_an_origin_is_answered_without_cors_headers() {
        // A native client sends no `Origin`; this is not a CORS request, and
        // the allow-list must not get in its way.
        let (client, state) = served();
        let token = session(&state).await;
        let response = client
            .get("/v1/me")
            .cookie("beam_session", &token)
            .send()
            .await;

        response.assert_status(StatusCode::OK);
        assert_eq!(response.header("access-control-allow-origin"), None);
    }
}
