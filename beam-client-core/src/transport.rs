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
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};

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

tokio::task_local! {
    /// Where the middleware leaves the problem document for the call in progress.
    ///
    /// Scoped to the future that made the request, rather than kept on the
    /// middleware. One shared slot was a race, not a store: the write happens after
    /// `response.bytes()` is awaited, so with two requests in flight on one client
    /// the second overwrote the first before the first read it -- and the reader
    /// had no way to notice. What a viewer saw was a title reporting another
    /// title's failure: `BeamErrors.kt` branches on `#source-file-missing`, so a
    /// plain 404 could be shown as "ask an administrator to rescan the library".
    ///
    /// Keying the slot by `tokio::task::try_id()` looked like the fix and was not.
    /// uniffi 0.32 wraps each foreign async call in `async_compat::Compat` and
    /// polls it inline on the FFI caller's thread -- there is no `tokio::spawn` --
    /// so the id is `None` for every Kotlin and Swift call, and all concurrent
    /// foreign calls shared one slot: exactly the race the key was meant to close.
    ///
    /// `LocalKey::scope` binds the value to the polls of one future, whatever
    /// drives it: a spawned task, a `JoinSet` entry, a bare `block_on`, or uniffi's
    /// inline poll. [`with_problem`] opens the scope and reads it back, and the
    /// middleware writes through [`tokio::task::LocalKey::try_with`], so a request
    /// made with no scope active -- `logout`'s best-effort revoke -- captures
    /// nothing rather than failing.
    static PROBLEM_SLOT: ProblemSlot;
}

/// The scope's value. Shared with the caller so the document can be read after
/// the scoped future has completed and dropped its own handle.
type ProblemSlot = Arc<Mutex<Option<ProblemDetail>>>;

/// Run `request` with a fresh problem slot in scope, and return whatever the
/// middleware left in it.
///
/// This is the one choke point every call through the generated client goes
/// through; [`TransportFailure::capture`] is the shape the façade uses.
pub(crate) async fn with_problem<F: Future>(request: F) -> (F::Output, Option<ProblemDetail>) {
    let slot: ProblemSlot = Arc::default();
    let output = PROBLEM_SLOT.scope(Arc::clone(&slot), request).await;
    let problem = slot.lock().expect("problem slot").take();
    (output, problem)
}

/// Attaches the session cookie, notices when the server rejects it, and hands
/// the problem document from a failed response to the call that made it.
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
            // Left in the caller's slot, where `with_problem` reads it back.
            // `None` for a body that is not a document -- a proxy's HTML page
            // -- so a later failure in the same scope cannot claim an earlier
            // one's. `Err(AccessError)` means no scope is active, which is a
            // request whose caller never reads the document; there is nowhere
            // to put it and nothing lost by not doing so.
            //
            // Read here rather than out of the generated error because spargen
            // gives each operation its own error enum -- `GetMediaDetailError`,
            // `DeleteLibraryError`, thirty-odd of them -- each holding
            // `Box<types::Problem>` behind a `StatusNNN` variant, and none of
            // them exposing an accessor. There is no generic way to reach the
            // body from `Error::Api`, so the middleware -- which sees every
            // response through one function -- reads it instead. That gap is
            // filed upstream as getkono/spargen#85 ("Generated per-operation
            // error enums expose no accessor for the problem document they
            // carry"); until a release carries it, this is where the document
            // comes from.
            let parsed = ProblemDetail::parse(&body);
            let _ = PROBLEM_SLOT.try_with(|slot| *slot.lock().expect("problem slot") = parsed);

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
    /// The problem document the response carried, where it carried one.
    ///
    /// Read out of the call's own scope by [`TransportFailure::capture`], so it
    /// is this failure's document and no other's.
    pub(crate) problem: Option<ProblemDetail>,
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
    /// Run one generated-client call and describe its failure, document and all.
    ///
    /// The only way the façade obtains a `TransportFailure`, so no call can
    /// forget to open the scope the middleware writes into.
    pub(crate) async fn capture<T, E: std::fmt::Display>(
        request: impl Future<Output = Result<T, crate::api::Error<E>>>,
    ) -> Result<T, Self> {
        let (outcome, problem) = with_problem(request).await;
        outcome.map_err(|error| Self::of(&error, problem))
    }

    fn of<E: std::fmt::Display>(
        error: &crate::api::Error<E>,
        problem: Option<ProblemDetail>,
    ) -> Self {
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
            problem,
        }
    }
}

/// The longest `Retry-After` a response is believed about.
///
/// One hour: beam-server's rate limiter answers in seconds, a proxy's
/// maintenance window in minutes, and anything beyond an hour is a misconfigured
/// origin -- or an `i64::MAX` a caller would otherwise have to add to a clock.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 60 * 60;

/// `Retry-After` as whole seconds, clamped to [`MAX_RETRY_AFTER_SECS`].
///
/// Only the delta-seconds form is read. RFC 9110 also permits an HTTP-date
/// (`Retry-After: Fri, 31 Dec 1999 23:59:59 GMT`), and that form is not
/// supported: beam-server never sends one, and guessing at a clock skew to
/// convert it would produce a worse answer than the caller's own default. It
/// reads as `None`, exactly like a header that is missing or garbage.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let seconds: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(seconds.min(MAX_RETRY_AFTER_SECS))
}

#[cfg(test)]
pub(crate) use canned::CannedBackend;

#[mutants::skip]
#[cfg(test)]
mod canned {
    /// A backend that answers every request with one canned response.
    ///
    /// The seam spargen provides for exactly this: no listener, no network, and
    /// the middleware under test is the real one running in the real chain.
    #[derive(Debug)]
    pub(crate) struct CannedBackend {
        pub(crate) status: u16,
        pub(crate) content_type: &'static str,
        pub(crate) body: &'static str,
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

    /// Runs one request through the real middleware over `backend`, with no
    /// problem scope of its own -- the caller decides whether one is open.
    ///
    /// The request is built directly rather than through a `reqwest::Client`:
    /// under this crate's TLS features `Client::new()` panics with "No provider
    /// set" unless `install_crypto_provider` has already run in the process,
    /// which made these tests pass in the full run and fail when run alone.
    async fn drive(
        middleware: &Arc<SessionMiddleware>,
        backend: CannedBackend,
    ) -> reqwest::Response {
        use crate::api::{HttpBackend, MiddlewareBackend};

        let chain = MiddlewareBackend::with_middlewares(
            Arc::new(backend),
            vec![Arc::clone(middleware) as Arc<dyn Middleware>],
        );
        let request = reqwest::Request::new(
            reqwest::Method::GET,
            "http://beam.invalid/v1/media/7"
                .parse()
                .expect("a literal URL parses"),
        );

        chain
            .execute(request)
            .await
            .expect("the canned backend answers")
    }

    /// `drive`, inside its own problem scope: the response and what it left.
    async fn drive_scoped(
        middleware: &Arc<SessionMiddleware>,
        backend: CannedBackend,
    ) -> (reqwest::Response, Option<ProblemDetail>) {
        with_problem(drive(middleware, backend)).await
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

        let (response, problem) = drive_scoped(
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

        let problem = problem.expect("the document was captured");
        assert_eq!(
            problem.type_uri,
            "https://beam.justinchung.net/reference/errors/#source-file-missing"
        );
        assert_eq!(
            problem.detail.as_deref(),
            Some("Source video file not found")
        );
    }

    /// Two requests in flight on one client each read their own document --
    /// in the shape the product actually runs.
    ///
    /// uniffi 0.32 polls each foreign async call inline on the caller's thread
    /// through `async_compat::Compat`; nothing is `tokio::spawn`ed, so every
    /// Kotlin and Swift call has the same `tokio::task::try_id()` -- `None`.
    /// Keying the document by task id therefore put all of them in one slot.
    /// `join!` reproduces that: both futures are polled by this one task, and
    /// the barrier holds both responses captured before either is read, which
    /// is exactly the interleaving that lost a document. `BeamErrors.kt`
    /// branches on `#source-file-missing`, so the loss showed a plain 404 as
    /// "ask an administrator to rescan the library".
    #[tokio::test]
    async fn concurrent_calls_polled_by_one_task_each_read_their_own_document() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let call = |body: &'static str| {
            let middleware = Arc::clone(&middleware);
            let barrier = Arc::clone(&barrier);
            with_problem(async move {
                drive(
                    &middleware,
                    CannedBackend {
                        status: 404,
                        content_type: "application/problem+json",
                        body,
                    },
                )
                .await;
                barrier.wait().await;
            })
        };

        let (((), first), ((), second)) = tokio::join!(
            call(
                r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
            ),
            call(
                r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404}"#,
            ),
        );

        assert_eq!(
            first.expect("the first caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#media-not-found"
        );
        assert_eq!(
            second.expect("the second caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#source-file-missing"
        );
    }

    /// The same two requests, each on a spawned task -- the shape `hydrate`
    /// produces through its `JoinSet`. The scope travels with the future, so
    /// it does not matter which task, or how many, poll it.
    #[tokio::test]
    async fn concurrent_spawned_tasks_each_read_their_own_document() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let spawn = |body: &'static str| {
            let middleware = Arc::clone(&middleware);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(with_problem(async move {
                drive(
                    &middleware,
                    CannedBackend {
                        status: 404,
                        content_type: "application/problem+json",
                        body,
                    },
                )
                .await;
                barrier.wait().await;
            }))
        };

        let one = spawn(
            r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
        );
        let two = spawn(
            r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404}"#,
        );

        let ((), first) = one.await.expect("the task completes");
        let ((), second) = two.await.expect("the task completes");

        assert_eq!(
            first.expect("the first caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#media-not-found"
        );
        assert_eq!(
            second.expect("the second caller has a document").type_uri,
            "https://beam.justinchung.net/reference/errors/#source-file-missing"
        );
    }

    /// A request made with no scope open -- `logout`'s best-effort revoke --
    /// still completes, and leaves nothing behind for a later scope to find.
    #[tokio::test]
    async fn a_request_outside_any_scope_captures_nothing_and_does_not_panic() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let response = drive(
            &middleware,
            CannedBackend {
                status: 404,
                content_type: "application/problem+json",
                body: r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
            },
        )
        .await;
        assert_eq!(response.status(), 404);

        // A scope opened afterwards starts empty: the unscoped write had
        // nowhere to land, and the slot is per scope rather than per client.
        let ((), later) = with_problem(async {}).await;
        assert!(later.is_none());
    }

    /// A failed response with no document clears the slot rather than leaving
    /// the previous document in it.
    ///
    /// Within one scope the second failure is the one being reported, and a
    /// gateway's HTML page carries no `type`. Leaving the earlier document in
    /// place would attach the wrong `code` to it.
    #[tokio::test]
    async fn a_failure_without_a_document_clears_an_earlier_one() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let ((), problem) = with_problem(async {
            drive(
                &middleware,
                CannedBackend {
                    status: 404,
                    content_type: "application/problem+json",
                    body: r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
                },
            )
            .await;
            drive(
                &middleware,
                CannedBackend {
                    status: 502,
                    content_type: "text/html",
                    body: "<html>502 Bad Gateway</html>",
                },
            )
            .await;
        })
        .await;

        assert!(problem.is_none(), "got {problem:?}");
    }

    #[tokio::test]
    async fn a_successful_response_is_not_buffered() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let (response, problem) = drive_scoped(
            &middleware,
            CannedBackend {
                status: 200,
                content_type: "application/json",
                body: r#"{"id":"7"}"#,
            },
        )
        .await;

        assert_eq!(response.status(), 200);
        assert!(problem.is_none());
    }

    /// The 401 flag and the problem capture are independent.
    ///
    /// A 401 from the session check carries `about:blank`, and the session
    /// machine still has to see it.
    #[tokio::test]
    async fn a_401_still_sets_the_flag_while_its_document_is_captured() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let (_, problem) = drive_scoped(
            &middleware,
            CannedBackend {
                status: 401,
                content_type: "application/problem+json",
                body: r#"{"type":"about:blank","status":401,"detail":"no session"}"#,
            },
        )
        .await;

        assert!(middleware.take_unauthorized());
        assert_eq!(problem.expect("captured").type_uri, ABOUT_BLANK);
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
    fn a_whitespace_only_detail_falls_back_to_the_generic_message() {
        // beam-server can send `"detail": ""` for a problem it has no words
        // for; a viewer shown a blank line has been told nothing.
        let document = problem("about:blank", Some("   \n\t"));
        match classify(404, Some(&document), None) {
            BeamError::NotFound { detail, .. } => {
                assert!(!detail.trim().is_empty(), "got {detail:?}");
            }
            other => panic!("expected not found, got {other:?}"),
        }
    }

    fn headers_with_retry_after(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            value.parse().expect("a header value"),
        );
        headers
    }

    #[test]
    fn retry_after_reads_delta_seconds_and_nothing_else() {
        assert_eq!(retry_after_secs(&headers_with_retry_after("30")), Some(30));
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("  30  ")),
            Some(30),
            "surrounding whitespace is not part of the value"
        );
        assert_eq!(
            retry_after_secs(&reqwest::header::HeaderMap::new()),
            None,
            "a missing header is not a zero-second wait"
        );
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("soon")),
            None,
            "garbage is not a number"
        );
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("-5")),
            None,
            "a negative delta is not a number of seconds"
        );
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("Fri, 31 Dec 1999 23:59:59 GMT")),
            None,
            "the HTTP-date form is deliberately unsupported"
        );
    }

    /// An absurd `Retry-After` is believed only up to the ceiling.
    ///
    /// The queue adds this to a clock; a header of `i64::MAX` seconds would
    /// otherwise overflow it, and a header of a year would park a resume
    /// point past its own retention window.
    #[test]
    fn retry_after_is_clamped_to_the_ceiling() {
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("9223372036854775807")),
            Some(MAX_RETRY_AFTER_SECS)
        );
        assert_eq!(
            retry_after_secs(&headers_with_retry_after("18446744073709551616")),
            None,
            "past u64 it is not a number at all"
        );
        assert_eq!(
            retry_after_secs(&headers_with_retry_after(&MAX_RETRY_AFTER_SECS.to_string())),
            Some(MAX_RETRY_AFTER_SECS),
            "the ceiling itself is not clamped below itself"
        );
    }

    /// A `reqwest::Error` without a client or a network: the status check on
    /// a hand-built response.
    fn a_reqwest_error() -> reqwest::Error {
        let response = ::http::Response::builder()
            .status(500)
            .body(String::new())
            .expect("a response builds");
        reqwest::Response::from(response)
            .error_for_status()
            .expect_err("a 500 is an error status")
    }

    /// A `reqwest::Error` that classifies as a decode failure, which
    /// `Error::from_reqwest` files under `Protocol`.
    fn a_reqwest_decode_error() -> reqwest::Error {
        let response = ::http::Response::builder()
            .status(200)
            .body("{".to_owned())
            .expect("a response builds");
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(reqwest::Response::from(response).json::<serde_json::Value>())
            .expect_err("`{` is not JSON")
    }

    /// Which of the three answers each generated failure gives.
    ///
    /// The status-bearing variants must surface the status and its
    /// `Retry-After`; everything with no response must land on the side of
    /// `Unreachable`/`Malformed` that its retry semantics call for, because
    /// `is_retryable` and therefore the progress queue follow from that.
    #[test]
    fn every_generated_failure_is_classified_by_what_a_retry_could_change() {
        use crate::api::{Error, ResponseValue, TimeoutKind, TransportError};

        let api: Error<String> = Error::Api(ResponseValue::new(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            headers_with_retry_after("12"),
            "slow down".to_owned(),
        ));
        let failure = TransportFailure::of(&api, None);
        assert_eq!(failure.kind, FailureKind::Answered(429));
        assert_eq!(failure.retry_after_secs, Some(12));

        let unexpected: Error<String> = Error::UnexpectedStatus {
            status: reqwest::StatusCode::IM_A_TEAPOT,
            headers: reqwest::header::HeaderMap::new(),
            body: bytes::Bytes::from_static(b"short and stout"),
        };
        let failure = TransportFailure::of(&unexpected, None);
        assert_eq!(failure.kind, FailureKind::Answered(418));
        assert_eq!(failure.retry_after_secs, None);

        let no_response: [Error<String>; 3] = [
            Error::Transport(TransportError::new(a_reqwest_error())),
            Error::Timeout(TimeoutKind::Total),
            Error::InterruptedBody(TransportError::new(a_reqwest_error())),
        ];
        for error in &no_response {
            let failure = TransportFailure::of(error, None);
            assert_eq!(failure.kind, FailureKind::Unreachable, "{error}");
            assert_eq!(failure.retry_after_secs, None);
            assert!(!failure.message.is_empty());
        }

        let protocol: Error<String> = Error::from_reqwest(a_reqwest_decode_error());
        assert!(matches!(protocol, Error::Protocol(_)), "{protocol:?}");
        assert_eq!(
            TransportFailure::of(&protocol, None).kind,
            FailureKind::Malformed
        );
    }

    /// The document handed to `of` is the failure's, verbatim: `capture` is
    /// what reads it from the scope, and nothing downstream re-derives it.
    #[tokio::test]
    async fn capture_attaches_the_scoped_document_to_the_failure() {
        let middleware = Arc::new(SessionMiddleware::new(Arc::new(SessionCookieHolder::new())));

        let outcome: Result<(), TransportFailure> = TransportFailure::capture(async {
            let response = drive(
                &middleware,
                CannedBackend {
                    status: 404,
                    content_type: "application/problem+json",
                    body: r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404}"#,
                },
            )
            .await;
            Err::<(), crate::api::Error<String>>(crate::api::Error::UnexpectedStatus {
                status: response.status(),
                headers: response.headers().clone(),
                body: response.bytes().await.expect("a body"),
            })
        })
        .await;

        let failure = outcome.expect_err("the canned 404 is a failure");
        assert_eq!(failure.kind, FailureKind::Answered(404));
        assert_eq!(
            failure.problem.expect("captured").type_uri,
            "https://beam.justinchung.net/reference/errors/#media-not-found"
        );

        let clean: Result<(), TransportFailure> = TransportFailure::capture(async {
            Err::<(), crate::api::Error<String>>(crate::api::Error::Timeout(
                crate::api::TimeoutKind::Total,
            ))
        })
        .await;
        assert!(
            clean.expect_err("a timeout is a failure").problem.is_none(),
            "a failure with no response has no document"
        );
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
        assert_eq!(
            TransportFailure::of(&decode, None).kind,
            FailureKind::Malformed
        );

        let unbuildable: Error<std::convert::Infallible> = Error::request_message("not a base URL");
        assert_eq!(
            TransportFailure::of(&unbuildable, None).kind,
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
