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
use std::collections::HashMap;
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

/// How many task slots the problem map holds before it starts evicting.
///
/// A task that fails and never reads its document -- `hydrate` drops the
/// failures of titles it could not fetch -- would otherwise leave an entry
/// behind for the life of the client. Eviction under pressure costs the
/// fallback message rather than the wrong one, which is the right way round.
const MAX_PENDING_PROBLEMS: usize = 64;

/// Attaches the session cookie, notices when the server rejects it, and keeps
/// the problem document from whatever failed.
#[derive(Debug)]
pub struct SessionMiddleware {
    cookie: Arc<SessionCookieHolder>,
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
    /// Keyed by the task that made the request.
    ///
    /// One shared slot was a race, not a store: the write happens after
    /// `response.bytes()` is awaited, so with two requests in flight on one
    /// client the second overwrites the first before the first reads it -- and
    /// the reader has no way to notice. `hydrate` puts concurrent
    /// `get_media_detail` calls on one backend through a `JoinSet`, and both
    /// mobile clients fan out on their home and detail screens, so this was
    /// reachable without any unusual caller. What a viewer saw was a title
    /// reporting another title's failure: `BeamErrors.kt` branches on
    /// `#source-file-missing`, so a plain 404 could be shown as "ask an
    /// administrator to rescan the library".
    ///
    /// Keying by task is what makes the association real. Every concurrent
    /// request is a separate tokio task -- spawned by `JoinSet`, or by uniffi
    /// for each foreign async call -- and `map_error` runs in the same task
    /// that made the request, so it reads its own document or none.
    problems: Arc<RwLock<HashMap<Option<tokio::task::Id>, ProblemDetail>>>,
}

impl SessionMiddleware {
    /// Wrap a cookie holder.
    #[must_use]
    pub fn new(cookie: Arc<SessionCookieHolder>) -> Self {
        Self {
            cookie,
            unauthorized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            problems: Arc::new(RwLock::new(HashMap::new())),
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

    /// The problem document from *this task's* last failed response.
    ///
    /// Read here rather than out of the generated error because spargen gives
    /// each operation its own error enum -- `GetMediaDetailError`,
    /// `DeleteLibraryError`, thirty-odd of them -- each holding
    /// `Box<types::Problem>` behind a `StatusNNN` variant, and none of them
    /// deriving `Serialize` or exposing an accessor. There is no generic way
    /// to reach the body from `Error::Api`, so the middleware -- which sees
    /// every response through one function -- reads it instead.
    ///
    /// That gap belongs upstream in spargen rather than here: a generated
    /// error that carries a problem document should be able to hand it back.
    /// Until a release does, this is where the document comes from.
    #[must_use]
    pub fn take_problem(&self) -> Option<ProblemDetail> {
        self.problems
            .write()
            .expect("problem lock")
            .remove(&tokio::task::try_id())
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
        let problems = Arc::clone(&self.problems);
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
            {
                // `None` outside a task -- a bare `block_on`, which the tests
                // use. Those cannot interleave two requests the way spawned
                // tasks can, so they share one slot and lose nothing by it.
                let id = tokio::task::try_id();
                let mut problems = problems.write().expect("problem lock");
                match ProblemDetail::parse(&body) {
                    Some(parsed) => {
                        // Bounded: a task that never reads its document would
                        // otherwise hold a slot forever.
                        if problems.len() >= MAX_PENDING_PROBLEMS
                            && !problems.contains_key(&id)
                            && let Some(&victim) = problems.keys().next()
                        {
                            problems.remove(&victim);
                        }
                        problems.insert(id, parsed);
                    }
                    // A response with no document clears this task's slot, so a
                    // later failure cannot claim an earlier one's.
                    None => {
                        problems.remove(&id);
                    }
                }
            }

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
        // 5xx is the server failing to handle a request it accepted, so the
        // same request may well succeed later. Anything else reaching here is
        // the request being refused -- 415 and 422 are declared on three
        // in-client operations -- and resending it unchanged fails the same
        // way. Decided once, here, so no client has to guess.
        other => BeamError::Server {
            status: other,
            retryable: other >= 500,
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
    /// What kind of failure this was, where no response arrived to speak for
    /// itself.
    pub(crate) kind: FailureKind,
    /// `Retry-After`, in whole seconds, when the response carried a usable one.
    pub(crate) retry_after_secs: Option<u64>,
    /// The transport's own description, used only when there is no response.
    pub(crate) message: String,
}

/// The three answers a failed request can give, before its body is read.
///
/// Split because `is_retryable` drives the progress queue, and its own doc
/// comment warns that a permanently-failing sample must not be able to occupy
/// that queue forever. Collapsing all of these into one retryable network error
/// is exactly how it would.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// A response arrived, with this status. Its body decides the rest.
    Answered(u16),
    /// No response arrived, and the same request could plausibly get one:
    /// connection refused, TLS handshake, timeout, a stream that stopped.
    Unreachable,
    /// The request or the response could not be made sense of: a base URL that
    /// will not build, a body that does not match the contract the client was
    /// generated from. Retrying reproduces it exactly.
    Malformed,
}

impl TransportFailure {
    pub(crate) fn of<E: std::fmt::Display>(error: &crate::api::Error<E>) -> Self {
        use crate::api::Error;

        let (kind, headers) = match error {
            Error::Api(value) => (
                FailureKind::Answered(value.status().as_u16()),
                Some(value.headers()),
            ),
            Error::UnexpectedStatus {
                status, headers, ..
            } => (FailureKind::Answered(status.as_u16()), Some(headers)),

            Error::Transport(_)
            | Error::Timeout(_)
            | Error::Redirect(_)
            | Error::InterruptedBody(_) => (FailureKind::Unreachable, None),

            // A URL that will not build and a body that will not decode are
            // both permanent: the identical request produces the identical
            // failure, so offering a retry would be a dead end.
            Error::RequestConstruction(_) | Error::Protocol(_) | Error::Decode { .. } => {
                (FailureKind::Malformed, None)
            }
        };

        Self {
            kind,
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
    /// Two requests in flight on one client each read their own document.
    ///
    /// This is the shape `hydrate` produces: a `JoinSet` of `get_media_detail`
    /// calls sharing one `MiddlewareBackend`. With a single shared slot the
    /// second write lands before the first reader gets there, so one caller
    /// took the other's `type` and the other took none -- and because
    /// `BeamErrors.kt` branches on `#source-file-missing`, a plain 404 could be
    /// shown as "ask an administrator to rescan the library".
    ///
    /// The barrier is what makes the failure deterministic rather than a race
    /// the test might win: both responses are captured before either is read,
    /// which is exactly the interleaving that loses a document.
    #[tokio::test]
    async fn concurrent_requests_each_read_their_own_problem_document() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let one = tokio::spawn({
            let middleware = Arc::clone(&middleware);
            let barrier = Arc::clone(&barrier);
            async move {
                drive(&middleware, CannedBackend {
                    status: 404,
                    content_type: "application/problem+json",
                    body: r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
                })
                .await;
                barrier.wait().await;
                middleware.take_problem()
            }
        });

        let two = tokio::spawn({
            let middleware = Arc::clone(&middleware);
            let barrier = Arc::clone(&barrier);
            async move {
                drive(&middleware, CannedBackend {
                    status: 404,
                    content_type: "application/problem+json",
                    body: r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404}"#,
                })
                .await;
                barrier.wait().await;
                middleware.take_problem()
            }
        });

        let first = one.await.expect("the task completes");
        let second = two.await.expect("the task completes");

        assert_eq!(
            first.expect("the first caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#media-not-found"
        );
        assert_eq!(
            second.expect("the second caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#source-file-missing"
        );
    }

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

    /// A failure with no response is not automatically worth retrying.
    ///
    /// `is_retryable` decides whether the playback-progress queue enqueues or
    /// drops, and its own doc comment says a permanently-failing sample must
    /// not be able to occupy that queue forever. A body that will not decode
    /// and a base URL that will not build both reproduce exactly on a retry,
    /// so they are `Protocol`, not a retryable `Network`.
    #[test]
    fn a_permanent_failure_is_not_classified_as_a_retryable_network_error() {
        use crate::api::Error;

        let decode: Error<std::convert::Infallible> = Error::Decode {
            path: "$.id".to_owned(),
            body: bytes::Bytes::from_static(b"{"),
            truncated: false,
        };
        assert_eq!(TransportFailure::of(&decode).kind, FailureKind::Malformed);

        let unbuildable: Error<std::convert::Infallible> = Error::request_message("not a base URL");
        assert_eq!(
            TransportFailure::of(&unbuildable).kind,
            FailureKind::Malformed
        );
    }

    #[test]
    fn a_401_is_reported_as_unauthenticated_for_the_caller_to_refine() {
        // Only the caller knows whether a session was in place, which is what
        // separates "sign in" from "your session expired".
        assert_eq!(classify(401, None, None), BeamError::Unauthenticated);
    }

    /// Where the retryability of a `Server` status is actually decided.
    ///
    /// It used to be derived three times -- once in Rust, once in Kotlin, once
    /// in Swift -- and the two clients answered "retryable" for every status,
    /// so a 415 or a 422 (declared on three in-client operations) offered a
    /// retry for a body the server refuses identically every time. The verdict
    /// is now set here and carried on the error, so no client derives it.
    #[test]
    fn a_server_status_carries_its_own_retryability() {
        for status in [500_u16, 502, 503] {
            let error = classify(status, None, None);
            assert!(
                matches!(
                    error,
                    BeamError::Server {
                        retryable: true,
                        ..
                    }
                ),
                "{status} should arrive retryable, got {error:?}"
            );
        }
        for status in [415_u16, 418, 422] {
            let error = classify(status, None, None);
            assert!(
                matches!(
                    error,
                    BeamError::Server {
                        retryable: false,
                        ..
                    }
                ),
                "{status} should arrive non-retryable, got {error:?}"
            );
        }
    }

    #[test]
    fn an_unmapped_status_is_preserved_rather_than_flattened() {
        let document = problem("about:blank", Some("teapot"));
        match classify(418, Some(&document), None) {
            BeamError::Server {
                status,
                retryable,
                detail,
                code,
            } => {
                assert_eq!(status, 418);
                assert_eq!(detail, "teapot");
                assert_eq!(code, ABOUT_BLANK);
                assert!(
                    !retryable,
                    "a 4xx reaching the Server variant is the request being refused"
                );
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }
}
