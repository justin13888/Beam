use salvo::http::StatusCode;
use salvo::prelude::*;
use salvo::test::TestClient;

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
    use super::*;

    #[handler]
    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn service() -> Service {
        Service::new(Router::new().goal(ok_handler)).hoop(cors_handler())
    }

    #[tokio::test]
    async fn a_preflight_allows_the_headers_the_player_and_client_send() {
        let response = TestClient::options("http://127.0.0.1:8000/")
            .add_header("origin", "https://beam.example.com", true)
            .add_header("access-control-request-method", "GET", true)
            .add_header("access-control-request-headers", "range", true)
            .send(&service())
            .await;

        assert!(
            response.status_code.unwrap_or(StatusCode::OK).is_success(),
            "the preflight was rejected: {:?}",
            response.status_code
        );
        let allowed = response
            .headers()
            .get("access-control-allow-headers")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            allowed.contains("range"),
            "without `range` the player cannot seek: {allowed}"
        );
        assert!(allowed.contains("content-type"), "{allowed}");
    }

    #[tokio::test]
    async fn credentials_are_allowed_so_the_session_cookie_is_sent() {
        let response = TestClient::get("http://127.0.0.1:8000/")
            .add_header("origin", "https://beam.example.com", true)
            .send(&service())
            .await;

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-credentials")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
            "cookie-only auth needs credentialed CORS"
        );
    }

    #[tokio::test]
    async fn the_range_response_headers_are_exposed_to_the_browser() {
        // A browser cannot read `content-range` off a cross-origin response
        // unless it is explicitly exposed, and the player needs it to know how
        // much of the file it just received.
        let response = TestClient::get("http://127.0.0.1:8000/")
            .add_header("origin", "https://beam.example.com", true)
            .send(&service())
            .await;

        let exposed = response
            .headers()
            .get("access-control-expose-headers")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        for header in ["accept-ranges", "content-length", "content-range"] {
            assert!(exposed.contains(header), "{header} not exposed: {exposed}");
        }
    }

    #[tokio::test]
    async fn the_origin_is_mirrored_rather_than_allow_listed() {
        // Documented posture, not an accident: `/v1` is protected by
        // `enforce_same_origin` instead. Pinning it here means a change to
        // either half has to be a deliberate change to both.
        let response = TestClient::get("http://127.0.0.1:8000/")
            .add_header("origin", "https://somewhere.else.example", true)
            .send(&service())
            .await;

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://somewhere.else.example"),
        );
    }
}
