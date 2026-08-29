//! CSRF defense-in-depth for cookie-authenticated requests (see ADR-0003).
//!
//! `SameSite=Lax` on `beam_session` is the primary defense: it already
//! blocks the browser from attaching the cookie to a cross-site POST/PUT/
//! DELETE at all. This hoop is a second layer for the cases that aren't
//! airtight (older browsers, a misconfigured intermediary) -- state-changing
//! requests must present an `Origin` (or `Referer`, as a fallback) matching
//! an allowed origin. A request with neither header is let through: some
//! legitimate non-browser clients (curl, mobile apps) send neither, and
//! those never carry the cookie in the first place -- SameSite is what
//! actually stops a browser-based attack.

use http::Method;
use salvo::prelude::*;

use crate::state::AppState;

/// Extracts `scheme://host[:port]` from an Origin or Referer header value,
/// discarding any path/query (Referer includes them; Origin never does).
fn extract_origin(value: &str) -> Option<String> {
    let scheme_end = value.find("://")? + 3;
    let authority_end = value[scheme_end..]
        .find('/')
        .map(|i| scheme_end + i)
        .unwrap_or(value.len());
    Some(value[..authority_end].to_string())
}

fn allowed_origins(state: &AppState) -> Vec<String> {
    let mut origins = vec![
        state.config.web_url.trim_end_matches('/').to_string(),
        state.config.server_url.trim_end_matches('/').to_string(),
    ];
    if let Some(extra) = &state.config.extra_allowed_origins {
        origins.extend(
            extra
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    origins
}

#[handler]
pub async fn enforce_same_origin(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if matches!(
        *req.method(),
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    ) {
        return;
    }

    let raw_origin = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.headers().get("referer").and_then(|v| v.to_str().ok()));

    let Some(raw_origin) = raw_origin else {
        // No Origin/Referer at all -- allow. SameSite=Lax already stops the
        // browser cross-site case; this header is only ever absent for
        // non-browser clients that don't carry the cookie anyway.
        return;
    };

    let Some(origin) = extract_origin(raw_origin) else {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Text::Plain("Malformed Origin/Referer header"));
        return;
    };

    let Ok(state) = depot.obtain::<AppState>() else {
        // Wired in by `create_router`; a miss is a router wiring bug. Fail
        // closed (this hoop guards state-changing requests) without panicking.
        tracing::error!("AppState missing from depot -- router wiring bug");
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Plain("Server state unavailable"));
        return;
    };
    if !allowed_origins(state)
        .iter()
        .any(|allowed| allowed == &origin)
    {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Text::Plain("Cross-origin request rejected"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_origin_from_bare_origin_header() {
        assert_eq!(
            extract_origin("http://localhost:5173"),
            Some("http://localhost:5173".to_string())
        );
    }

    #[test]
    fn extracts_origin_from_referer_with_path() {
        assert_eq!(
            extract_origin("http://localhost:5173/library/some/deep/path?x=1"),
            Some("http://localhost:5173".to_string())
        );
    }

    #[test]
    fn rejects_malformed_value_without_scheme_separator() {
        assert_eq!(extract_origin("not-a-url"), None);
    }

    #[test]
    fn does_not_prefix_match_a_lookalike_host() {
        // A naive `starts_with` check would let this slip through; exact
        // equality on the extracted origin must not.
        let evil = extract_origin("http://localhost:5173.evil.example/path").unwrap();
        assert_ne!(evil, "http://localhost:5173");
    }
}

/// Subcutaneous tests for the enforcement itself.
///
/// The tests above cover `extract_origin` in isolation; these drive the hoop
/// with a real `AppState` in the depot, which is the only way to reach the
/// allow-list and the decision built on it. This is a CSRF control: when it
/// stops working, nothing observable changes until someone exploits it.
#[cfg(test)]
mod enforcement_tests {
    use salvo::prelude::*;
    use salvo::test::TestClient;

    use crate::routes::test_support::make_app_state_with;

    #[handler]
    async fn ok_handler() -> &'static str {
        "ok"
    }

    /// A service whose only route is state-changing, behind the hoop.
    fn service(extra_allowed_origins: Option<&str>) -> Service {
        let extra = extra_allowed_origins.map(str::to_string);
        let state = make_app_state_with(
            move |config| crate::config::ServerConfig {
                web_url: "http://localhost:5173".to_string(),
                server_url: "http://localhost:8000".to_string(),
                extra_allowed_origins: extra,
                ..config
            },
            std::sync::Arc::new(beam_domain::services::RealClock),
        );
        Service::new(
            Router::new()
                .hoop(affix_state::inject(state))
                .hoop(super::enforce_same_origin)
                .push(Router::with_path("thing").post(ok_handler)),
        )
    }

    async fn post_with_origin(service: &Service, header: &'static str, value: &str) -> StatusCode {
        TestClient::post("http://localhost/thing")
            .add_header(header, value.to_string(), true)
            .send(service)
            .await
            .status_code
            .unwrap_or(StatusCode::OK)
    }

    #[tokio::test]
    async fn the_configured_web_and_server_origins_are_both_accepted() {
        let service = service(None);
        for origin in ["http://localhost:5173", "http://localhost:8000"] {
            assert_eq!(
                post_with_origin(&service, "Origin", origin).await,
                StatusCode::OK,
                "{origin} is one of this deployment's own origins"
            );
        }
    }

    #[tokio::test]
    async fn an_unrecognised_origin_is_rejected() {
        assert_eq!(
            post_with_origin(&service(None), "Origin", "https://evil.example").await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_lookalike_host_is_rejected() {
        // The comparison is exact equality on the extracted origin; a prefix
        // or substring check would let this through.
        for lookalike in [
            "http://localhost:5173.evil.example",
            "http://evil.example/?x=http://localhost:5173",
            "https://localhost:5173",
            "http://localhost:51730",
        ] {
            assert_eq!(
                post_with_origin(&service(None), "Origin", lookalike).await,
                StatusCode::FORBIDDEN,
                "{lookalike} must not pass for an allowed origin"
            );
        }
    }

    #[tokio::test]
    async fn extra_allowed_origins_are_honoured_and_normalised() {
        // Operators write these by hand, so trailing slashes and spaces are
        // expected; an unlisted origin must still be refused.
        let service = service(Some(" https://beam.example.com/ , https://other.example "));
        assert_eq!(
            post_with_origin(&service, "Origin", "https://beam.example.com").await,
            StatusCode::OK
        );
        assert_eq!(
            post_with_origin(&service, "Origin", "https://other.example").await,
            StatusCode::OK
        );
        assert_eq!(
            post_with_origin(&service, "Origin", "https://third.example").await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn referer_is_used_when_there_is_no_origin_header() {
        let service = service(None);
        assert_eq!(
            post_with_origin(&service, "Referer", "http://localhost:5173/library/x?y=1").await,
            StatusCode::OK
        );
        assert_eq!(
            post_with_origin(&service, "Referer", "https://evil.example/x").await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_malformed_header_is_rejected_rather_than_ignored() {
        assert_eq!(
            post_with_origin(&service(None), "Origin", "not-a-url").await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_request_with_no_origin_or_referer_is_allowed() {
        // Documented in the module header: non-browser clients send neither
        // and never carry the cookie; SameSite=Lax is what stops the browser
        // case. Pinned so the decision cannot change silently.
        let status = TestClient::post("http://localhost/thing")
            .send(&service(None))
            .await
            .status_code
            .unwrap_or(StatusCode::OK);
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn safe_methods_are_never_checked() {
        // GET/HEAD/OPTIONS/TRACE change no state, and blocking them would
        // break cross-origin reads the CORS layer is there to allow.
        let service = Service::new(
            Router::new()
                .hoop(affix_state::inject(make_app_state_with(
                    |config| config,
                    std::sync::Arc::new(beam_domain::services::RealClock),
                )))
                .hoop(super::enforce_same_origin)
                .push(Router::with_path("thing").get(ok_handler)),
        );

        let status = TestClient::get("http://localhost/thing")
            .add_header("Origin", "https://evil.example", true)
            .send(&service)
            .await
            .status_code
            .unwrap_or(StatusCode::OK);
        assert_eq!(status, StatusCode::OK);
    }
}
