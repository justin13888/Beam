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

/// Attaches the session cookie, notices when the server rejects it, and keeps
/// the problem document from whatever failed.
#[derive(Debug)]
pub struct SessionMiddleware {
    cookie: Arc<SessionCookieHolder>,
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
    problem: Arc<RwLock<Option<ProblemDetail>>>,
}

impl SessionMiddleware {
    /// Wrap a cookie holder.
    #[must_use]
    pub fn new(cookie: Arc<SessionCookieHolder>) -> Self {
        Self {
            cookie,
            unauthorized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            problem: Arc::new(RwLock::new(None)),
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

    /// The problem document from the last failed response, if it carried one.
    ///
    /// Read here rather than out of the generated error because spargen gives
    /// each operation its own error enum -- `GetMediaDetailError`,
    /// `DeleteLibraryError`, thirty-odd of them -- and `ResponseValue` drops
    /// the raw body once it has decoded one. Reaching the `type` through those
    /// would mean a hand-written table over generated names, which drifts the
    /// moment an operation is added. The middleware sees every response
    /// through one function instead, so this needs nothing per operation.
    #[must_use]
    pub fn take_problem(&self) -> Option<ProblemDetail> {
        self.problem.write().expect("problem lock").take()
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
        let problem = Arc::clone(&self.problem);
        Box::pin(async move {
            let response = next.run(request).await?;
            let status = response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED {
                unauthorized.store(true, std::sync::atomic::Ordering::SeqCst);
            }

            if !status.is_client_error() && !status.is_server_error() {
                return Ok(response);
            }

            // Buffering happens only on a failure, and a problem document is
            // small by construction. The bytes are handed straight back on a
            // rebuilt response, so the generated client still decodes its own
            // typed error from exactly what the server sent.
            let headers = response.headers().clone();
            let body = response
                .bytes()
                .await
                .map_err(crate::api::TransportError::new)?;
            *problem.write().expect("problem lock") = ProblemDetail::parse(&body);

            let mut rebuilt = ::http::Response::new(body);
            *rebuilt.status_mut() = status;
            *rebuilt.headers_mut() = headers;
            Ok(reqwest::Response::from(rebuilt))
        })
    }
}

/// The two members of an RFC 9457 problem document a caller acts on.
///
/// `title` duplicates what `type` already identifies and `instance` names the
/// occurrence, so neither is kept: one belongs in a log, the other in nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemDetail {
    /// The stable, machine-readable identifier. Required by RFC 9457 and by
    /// the schema beam-server publishes.
    pub type_uri: String,
    /// The occurrence-specific explanation, for a person.
    pub detail: Option<String>,
}

impl ProblemDetail {
    /// Read a problem document out of a response body.
    ///
    /// `None` for anything that is not one -- a proxy's HTML error page, a
    /// gateway's plain text. Those never came from beam-server, so there is no
    /// document to find and the body itself is the only explanation there is.
    fn parse(body: &[u8]) -> Option<Self> {
        #[derive(serde::Deserialize)]
        struct Wire {
            #[serde(rename = "type")]
            type_uri: String,
            detail: Option<String>,
        }

        let wire: Wire = serde_json::from_slice(body).ok()?;
        Some(Self {
            type_uri: wire.type_uri,
            detail: wire.detail,
        })
    }
}

/// Turn a failed response into the core's error taxonomy.
///
/// **The status picks the variant, and the problem type rides along.** That
/// split is deliberate: HTTP semantics already say what a 404 means and what a
/// caller may do about it, so choosing the variant from the status needs no
/// table for anyone to maintain. The `type` is what beam-server adds on top --
/// *which* 404 this is -- and it is carried through opaquely rather than
/// matched on, so a code added on the server reaches a caller without a change
/// here.
///
/// `code` is `about:blank` when the response carried no problem document, and
/// when the framework answered rather than the application: the 401 from the
/// session check, the 429 from the rate limiter, the 404 for a URL matching no
/// route. RFC 9457 gives that exact reading -- the status code is the whole
/// story -- so it is an answer rather than a gap.
#[must_use]
pub fn classify(
    status: u16,
    problem: Option<&ProblemDetail>,
    retry_after_secs: Option<u64>,
) -> BeamError {
    let code = problem.map_or_else(
        || ABOUT_BLANK.to_owned(),
        |problem| problem.type_uri.clone(),
    );
    let detail = problem
        .and_then(|problem| problem.detail.clone())
        .filter(|detail| !detail.trim().is_empty())
        .unwrap_or_else(|| "The server did not explain the failure".to_owned());

    match status {
        400 => BeamError::BadRequest { detail, code },
        // The caller decides between Unauthenticated and SessionExpired: only
        // it knows whether a session was in place when this was sent.
        401 => BeamError::Unauthenticated,
        403 => BeamError::Forbidden { detail, code },
        404 => BeamError::NotFound { detail, code },
        429 => BeamError::RateLimited {
            // beam-server sends Retry-After, but a proxy in between may not.
            retry_after_secs: retry_after_secs.unwrap_or(60),
        },
        other => BeamError::Server {
            status: other,
            detail,
            code,
        },
    }
}

/// RFC 9457's "the status code is the whole story".
pub const ABOUT_BLANK: &str = "about:blank";

/// What a generated transport error says, independent of which operation it
/// came from.
///
/// `api::Error<E>` is generic over each operation's own error enum, but the two
/// variants that carry a response expose the status and headers without naming
/// `E` at all -- so this reads both generically and needs nothing per
/// operation. Only the message needs `E: Display`, which every generated error
/// enum implements.
#[derive(Debug)]
pub(crate) struct TransportFailure {
    /// The response status, where the request reached a response.
    ///
    /// `None` is what actually distinguishes a network failure from a server
    /// one: no response arrived, so there is no status to reason about and no
    /// problem document to read.
    pub(crate) status: Option<u16>,
    /// `Retry-After`, in whole seconds, when the response carried a usable one.
    pub(crate) retry_after_secs: Option<u64>,
    /// The transport's own description, used only when there is no response.
    pub(crate) message: String,
}

impl TransportFailure {
    pub(crate) fn of<E: std::fmt::Display>(error: &crate::api::Error<E>) -> Self {
        use crate::api::Error;

        let (status, headers) = match error {
            Error::Api(value) => (Some(value.status()), Some(value.headers())),
            Error::UnexpectedStatus {
                status, headers, ..
            } => (Some(*status), Some(headers)),
            // No response arrived: connection refused, TLS failure, timeout,
            // redirect exhaustion, a body that would not decode.
            Error::RequestConstruction(_)
            | Error::Transport(_)
            | Error::Timeout(_)
            | Error::Protocol(_)
            | Error::Redirect(_)
            | Error::Decode { .. }
            | Error::InterruptedBody(_) => (None, None),
        };

        Self {
            status: status.map(|status| status.as_u16()),
            retry_after_secs: headers.and_then(retry_after_secs),
            message: error.to_string(),
        }
    }
}

/// `Retry-After` as whole seconds.
///
/// Only the delta-seconds form is read. RFC 9110 also permits an HTTP-date, but
/// beam-server never sends one, and guessing at a clock skew to convert it
/// would produce a worse answer than the caller's own default.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
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

    /// A backend that answers every request with one canned response.
    ///
    /// The seam spargen provides for exactly this: no listener, no network, and
    /// the middleware under test is the real one running in the real chain.
    #[derive(Debug)]
    struct CannedBackend {
        status: u16,
        content_type: &'static str,
        body: &'static str,
    }

    impl crate::api::HttpBackend for CannedBackend {
        fn execute(&self, _request: reqwest::Request) -> crate::api::ExecuteFuture<'_> {
            let response = ::http::Response::builder()
                .status(self.status)
                .header("content-type", self.content_type)
                .body(self.body.to_owned())
                .expect("a canned response is well-formed");
            Box::pin(async move { Ok(reqwest::Response::from(response)) })
        }
    }

    /// Runs one request through the real middleware over `backend`.
    async fn drive(
        middleware: &Arc<SessionMiddleware>,
        backend: CannedBackend,
    ) -> reqwest::Response {
        use crate::api::{HttpBackend, MiddlewareBackend};

        let chain = MiddlewareBackend::with_middlewares(
            Arc::new(backend),
            vec![Arc::clone(middleware) as Arc<dyn Middleware>],
        );
        let request = reqwest::Client::new()
            .get("http://beam.invalid/v1/media/7")
            .build()
            .expect("a request builds");

        chain
            .execute(request)
            .await
            .expect("the canned backend answers")
    }

    /// The problem document is captured, and the response still reaches the
    /// generated client intact.
    ///
    /// Both halves matter. Capturing it is what gives a caller the `type`;
    /// handing the same bytes back is what keeps the generated client able to
    /// decode its own typed error from them. Buffering without rebuilding
    /// would trade one bug for a worse one.
    #[tokio::test]
    async fn a_problem_document_is_captured_and_the_body_still_arrives() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let response = drive(
            &middleware,
            CannedBackend {
                status: 404,
                content_type: "application/problem+json",
                body: r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404,"detail":"Source video file not found"}"#,
            },
        )
        .await;

        assert_eq!(response.status(), 404);
        assert!(
            response
                .text()
                .await
                .expect("a body")
                .contains("source-file-missing"),
            "the generated client decodes from these bytes, so they must survive the capture"
        );

        let problem = middleware
            .take_problem()
            .expect("the document was captured");
        assert_eq!(
            problem.type_uri,
            "https://beam.justinchung.net/reference/errors/#source-file-missing"
        );
        assert_eq!(
            problem.detail.as_deref(),
            Some("Source video file not found")
        );

        assert!(
            middleware.take_problem().is_none(),
            "taking must clear, or one failure would explain every later one"
        );
    }

    /// A success is passed through untouched: no buffering, nothing captured.
    #[tokio::test]
    async fn a_successful_response_is_not_buffered() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let response = drive(
            &middleware,
            CannedBackend {
                status: 200,
                content_type: "application/json",
                body: r#"{"id":"7"}"#,
            },
        )
        .await;

        assert_eq!(response.status(), 200);
        assert!(middleware.take_problem().is_none());
    }

    /// The 401 flag and the problem capture are independent.
    ///
    /// A 401 from the session check carries `about:blank`, and the session
    /// machine still has to see it.
    #[tokio::test]
    async fn a_401_still_sets_the_flag_while_its_document_is_captured() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        drive(
            &middleware,
            CannedBackend {
                status: 401,
                content_type: "application/problem+json",
                body: r#"{"type":"about:blank","status":401,"detail":"no session"}"#,
            },
        )
        .await;

        assert!(middleware.take_unauthorized());
        assert_eq!(
            middleware.take_problem().expect("captured").type_uri,
            ABOUT_BLANK
        );
    }

    /// The bytes beam-server actually puts on the wire for a missing title.
    fn problem(type_uri: &str, detail: Option<&str>) -> ProblemDetail {
        ProblemDetail {
            type_uri: type_uri.to_owned(),
            detail: detail.map(str::to_owned),
        }
    }

    #[test]
    fn a_problem_document_yields_its_detail_and_its_type() {
        let document = ProblemDetail::parse(
            br#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found",
                 "title":"Media not found","status":404,"detail":"media 7 not found"}"#,
        )
        .expect("a problem document parses");

        assert_eq!(
            classify(404, Some(&document), None),
            BeamError::NotFound {
                detail: "media 7 not found".to_owned(),
                code: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
            }
        );
    }

    /// The whole point of carrying `code`: two 404s a viewer must be told
    /// about differently.
    ///
    /// `media-not-found` is the viewer asking for something that is not there.
    /// `source-file-missing` means the catalogue and the disk have diverged,
    /// which no amount of retrying fixes and which an operator has to act on.
    /// Status alone cannot separate them, which is what issue #123 opened with.
    #[test]
    fn two_404s_are_distinguishable_by_code() {
        let missing_title = classify(404, Some(&problem(".../#media-not-found", Some("a"))), None);
        let missing_file = classify(
            404,
            Some(&problem(".../#source-file-missing", Some("b"))),
            None,
        );

        let (BeamError::NotFound { code: first, .. }, BeamError::NotFound { code: second, .. }) =
            (&missing_title, &missing_file)
        else {
            panic!("both are 404s: {missing_title:?} / {missing_file:?}");
        };
        assert_ne!(first, second);
    }

    #[test]
    fn a_response_with_no_problem_document_is_about_blank() {
        // A proxy's error page, a gateway's plain text: never beam-server, so
        // there is no type to report and the status is the whole story.
        assert!(ProblemDetail::parse(b"<html>502 Bad Gateway</html>").is_none());

        match classify(502, None, None) {
            BeamError::Server { status, code, .. } => {
                assert_eq!(status, 502);
                assert_eq!(code, ABOUT_BLANK);
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn a_problem_document_without_a_detail_still_produces_a_usable_message() {
        // The pre-Kynos extractor looked for an `{"error": ...}` key that no
        // longer exists and fell back to the raw body, so a viewer was shown
        // `{"type":"...","status":500}`.
        let document = ProblemDetail::parse(br#"{"type":"about:blank","status":500}"#)
            .expect("a problem document with no detail is still one");

        match classify(500, Some(&document), None) {
            BeamError::Server { detail, .. } => {
                assert!(!detail.is_empty());
                assert!(
                    !detail.contains('{'),
                    "a viewer must not be shown raw JSON: {detail}"
                );
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn rate_limiting_uses_retry_after_when_the_server_sent_one() {
        assert_eq!(
            classify(429, None, Some(30)),
            BeamError::RateLimited {
                retry_after_secs: 30
            }
        );
    }

    #[test]
    fn rate_limiting_without_retry_after_still_backs_off() {
        // A proxy may strip the header; retrying immediately would just earn
        // another 429.
        match classify(429, None, None) {
            BeamError::RateLimited { retry_after_secs } => assert!(retry_after_secs > 0),
            other => panic!("expected rate limiting, got {other:?}"),
        }
    }

    #[test]
    fn a_401_is_reported_as_unauthenticated_for_the_caller_to_refine() {
        // Only the caller knows whether a session was in place, which is what
        // separates "sign in" from "your session expired".
        assert_eq!(classify(401, None, None), BeamError::Unauthenticated);
    }

    #[test]
    fn an_unmapped_status_is_preserved_rather_than_flattened() {
        let document = problem("about:blank", Some("teapot"));
        match classify(418, Some(&document), None) {
            BeamError::Server {
                status,
                detail,
                code,
            } => {
                assert_eq!(status, 418);
                assert_eq!(detail, "teapot");
                assert_eq!(code, ABOUT_BLANK);
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }
}
