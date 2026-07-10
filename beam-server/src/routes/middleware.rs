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
