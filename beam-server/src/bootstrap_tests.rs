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
    /// `AppState` to be exercised.
    fn client() -> TestClient<()> {
        let service = Router::new()
            .mount(kynos::routes![probe, probe_patch, probe_delete])
            .intercept(cors_policy())
            .build(())
            .expect("the probe router describes itself");

        TestClient::new(service)
    }

    /// A preflight proposing `method`, from an origin that is not ours.
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
            BTreeSet::from(["GET", "PATCH", "DELETE"]),
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

    #[tokio::test]
    async fn the_origin_is_mirrored_rather_than_allow_listed() {
        // Documented posture, not an accident: `/v1` is protected by
        // `EnforceSameOrigin` instead. Pinning it here means a change to
        // either half has to be a deliberate change to both.
        let response = client()
            .get("/thing")
            .header("origin", "https://somewhere.else.example")
            .send()
            .await;

        assert_eq!(
            response.header("access-control-allow-origin"),
            Some("https://somewhere.else.example"),
        );
    }
}
