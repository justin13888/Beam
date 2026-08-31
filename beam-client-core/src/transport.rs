//! Getting the session cookie onto every request.
//!
//! `beam-server` reads exactly one credential -- the `beam_session` cookie --
//! and the exported spec declares no security schemes at all, so the generated
//! client has no auth parameter to fill in. The credential therefore has to be
//! attached below the generated code, at the transport seam.
//!
//! spargen offers two places to do that. A replacement [`HttpBackend`] would
//! mean reimplementing request execution; a [`Middleware`] wraps the stock
//! `ReqwestBackend` and gets to inspect the response as well, which is what
//! makes a mid-session 401 observable in one place instead of at every call
//! site. The middleware is the smaller and better-placed of the two.

use crate::api::{ExecuteFuture, Middleware, Next};
use crate::error::BeamError;
use std::sync::{Arc, RwLock};

/// The cookie `beam-server` issues and reads. Defined in
/// `beam-server/src/routes/api_error.rs`.
pub const SESSION_COOKIE: &str = "beam_session";

/// The current session cookie for one server, swappable at runtime.
///
/// Runtime-swappable rather than baked into the `reqwest::Client`'s default
/// headers, because the credential changes on login, logout, and expiry --
/// and rebuilding the client for each would discard the connection pool and
/// TLS session cache that Media3's neighbouring range requests benefit from.
#[derive(Debug, Default)]
pub struct SessionCookieHolder {
    cookie: RwLock<Option<String>>,
}

impl SessionCookieHolder {
    /// An empty holder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a cookie value, replacing any current one.
    pub fn set(&self, value: &str) {
        *self.cookie.write().expect("cookie lock") = Some(value.to_owned());
    }

    /// Remove the cookie, so requests go out unauthenticated.
    pub fn clear(&self) {
        *self.cookie.write().expect("cookie lock") = None;
    }

    /// The current value, if any.
    #[must_use]
    pub fn get(&self) -> Option<String> {
        self.cookie.read().expect("cookie lock").clone()
    }

    /// Whether a cookie is installed.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.cookie.read().expect("cookie lock").is_some()
    }
}

/// Attaches the session cookie, and notices when the server rejects it.
#[derive(Debug)]
pub struct SessionMiddleware {
    cookie: Arc<SessionCookieHolder>,
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
}

impl SessionMiddleware {
    /// Wrap a cookie holder.
    #[must_use]
    pub fn new(cookie: Arc<SessionCookieHolder>) -> Self {
        Self {
            cookie,
            unauthorized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Whether a 401 has been seen since this was last cleared.
    ///
    /// A flag rather than a callback: the middleware runs inside the request
    /// future, and driving the session machine from there would mean taking
    /// its lock while a request is in flight.
    #[must_use]
    pub fn take_unauthorized(&self) -> bool {
        self.unauthorized
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

impl Middleware for SessionMiddleware {
    fn handle<'a>(&'a self, mut request: reqwest::Request, next: Next<'a>) -> ExecuteFuture<'a> {
        if let Some(value) = self.cookie.get()
            && let Ok(header) =
                reqwest::header::HeaderValue::from_str(&format!("{SESSION_COOKIE}={value}"))
        {
            request
                .headers_mut()
                .insert(reqwest::header::COOKIE, header);
        }

        // Deliberately sets neither Origin nor Referer. beam-server's CSRF
        // hoop rejects an unsafe method whose Origin does not match, but
        // explicitly allows a request carrying neither -- the branch its own
        // comment describes as being for "legitimate non-browser clients
        // (curl, mobile apps)". Sending an Origin would put us in the
        // *checked* path with a value that cannot match.
        let unauthorized = Arc::clone(&self.unauthorized);
        Box::pin(async move {
            let response = next.run(request).await?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                unauthorized.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(response)
        })
    }
}

/// Turn an HTTP status and body into the core's error taxonomy.
///
/// The spec declares every `/v1` error as `text/plain` because `ApiError`'s
/// `ToResponses` derive names `String` bodies, while `ApiError`'s `Writer`
/// actually renders `{"error": "..."}`. Both shapes therefore reach a client,
/// and the JSON one is tried first with the raw text as the fallback.
#[must_use]
pub fn classify(status: u16, body: &str, retry_after_secs: Option<u64>) -> BeamError {
    let detail = extract_message(body);
    match status {
        400 => BeamError::BadRequest { detail },
        // The caller decides between Unauthenticated and SessionExpired: only
        // it knows whether a session was in place when this was sent.
        401 => BeamError::Unauthenticated,
        403 => BeamError::Forbidden { detail },
        404 => BeamError::NotFound { detail },
        429 => BeamError::RateLimited {
            // beam-server sends Retry-After, but a proxy in between may not.
            retry_after_secs: retry_after_secs.unwrap_or(60),
        },
        other => BeamError::Server {
            status: other,
            detail,
        },
    }
}

/// Pull a human-readable message out of either error-body shape.
fn extract_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "The server did not explain the failure".to_owned();
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cookie_can_be_installed_replaced_and_cleared() {
        let holder = SessionCookieHolder::new();
        assert!(!holder.is_set());

        holder.set("abc");
        assert_eq!(holder.get().as_deref(), Some("abc"));

        holder.set("def");
        assert_eq!(holder.get().as_deref(), Some("def"));

        holder.clear();
        assert!(!holder.is_set());
        assert_eq!(holder.get(), None);
    }

    #[test]
    fn the_unauthorized_flag_is_consumed_when_taken() {
        let middleware = SessionMiddleware::new(Arc::new(SessionCookieHolder::new()));
        assert!(!middleware.take_unauthorized());

        middleware
            .unauthorized
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(middleware.take_unauthorized());
        assert!(
            !middleware.take_unauthorized(),
            "taking must clear, or one 401 would expire every later session"
        );
    }

    #[test]
    fn a_json_error_body_yields_its_message() {
        // What beam-server's ApiError::write actually sends.
        let error = classify(404, r#"{"error":"Media not found"}"#, None);
        assert_eq!(
            error,
            BeamError::NotFound {
                detail: "Media not found".to_owned()
            }
        );
    }

    #[test]
    fn a_plain_text_error_body_is_used_as_the_message() {
        // What the stream and OIDC handlers actually send, and what the spec
        // claims every endpoint sends.
        let error = classify(403, "Admin access required", None);
        assert_eq!(
            error,
            BeamError::Forbidden {
                detail: "Admin access required".to_owned()
            }
        );
    }

    #[test]
    fn an_empty_body_still_produces_a_usable_message() {
        let error = classify(500, "", None);
        match error {
            BeamError::Server { status, detail } => {
                assert_eq!(status, 500);
                assert!(!detail.is_empty());
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn json_that_is_not_the_error_envelope_falls_back_to_the_raw_body() {
        let error = classify(400, r#"{"detail":"nope"}"#, None);
        assert_eq!(
            error,
            BeamError::BadRequest {
                detail: r#"{"detail":"nope"}"#.to_owned()
            }
        );
    }

    #[test]
    fn rate_limiting_uses_retry_after_when_the_server_sent_one() {
        assert_eq!(
            classify(429, "", Some(30)),
            BeamError::RateLimited {
                retry_after_secs: 30
            }
        );
    }

    #[test]
    fn rate_limiting_without_retry_after_still_backs_off() {
        // A proxy may strip the header; retrying immediately would just earn
        // another 429.
        match classify(429, "", None) {
            BeamError::RateLimited { retry_after_secs } => assert!(retry_after_secs > 0),
            other => panic!("expected rate limiting, got {other:?}"),
        }
    }

    #[test]
    fn a_401_is_reported_as_unauthenticated_for_the_caller_to_refine() {
        // Only the caller knows whether a session was in place, which is what
        // separates "sign in" from "your session expired".
        assert_eq!(classify(401, "", None), BeamError::Unauthenticated);
    }

    #[test]
    fn an_unmapped_status_is_preserved_rather_than_flattened() {
        match classify(418, "teapot", None) {
            BeamError::Server { status, detail } => {
                assert_eq!(status, 418);
                assert_eq!(detail, "teapot");
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }
}
