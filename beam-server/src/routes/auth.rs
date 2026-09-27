//! OIDC BFF endpoints (see ADR-0003): `login`/`callback` drive the
//! Authorization Code + PKCE round-trip, `device`/`device/token` drive the
//! RFC 8628 device authorization grant for a native client with no browser
//! (ADR-0017), and `me`/`logout`/`logout-all`/`sessions`/`sessions/{id}`
//! operate on the resulting `beam_session` credential -- the sole credential
//! beam-server issues. `login`/`callback`/`device*` are mounted under
//! `/v1/auth/*` and the rest at the top level (`/v1/me`, `/v1/logout`, ...).
//!
//! No client ever sees an IdP token. A browser holds `beam_session` as an
//! httpOnly, `SameSite=Lax` cookie; a device-grant client receives the same
//! opaque value in the poll's body and presents it as that cookie.
//!
//! This module lived in `beam-auth` until the Kynos migration. ADR-0010
//! requires the HTTP adapter to sit in `beam-server` so `beam-auth` stays
//! transport-independent, and moving it is what let that crate drop its
//! framework dependency entirely.
//!
//! Two shapes changed with the framework. Sessions are resolved by
//! `SessionAuth` in the signature rather than a `require_web_session` helper in
//! the body, so the requirement reaches the document. And dependencies arrive
//! through `Inject<T>`, so the `MissingDependency` marker and its 500 -- which
//! existed only because `depot.obtain::<T>()` could fail at run time -- are
//! gone: a missing dependency is now a compile error.

use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::device_auth_store::{
    Claim, DeviceAuthStore, NewDeviceAuth, generate_handle, hash_handle,
};
use beam_auth::utils::login::{
    ClientContext, LoginError, MintedSession, client_ip, complete_login,
};
use beam_auth::utils::models::User;
use beam_auth::utils::oidc::{DevicePoll, OidcClient, OidcError};
use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::pending_auth_store::{PendingAuth, PendingAuthStore};
use beam_auth::utils::repository::UserRepository;
use beam_auth::utils::session_store::{SessionError, SessionStore};
use kynos::prelude::*;
use kynos::response::cookie::{Cookie, SameSite};
use kynos::response::headers::WithHeaders;
use kynos::response::status::{NoContent, Redirect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::routes::api_error::{InternalError, SESSION_COOKIE, SessionAuth};
use crate::routes::tags::Auth;

const STATE_COOKIE: &str = "beam_oidc_state";
const STATE_TTL_SECS: u64 = 600; // 10 minutes to complete the round trip

/// The longest a device login may stay open, whatever the IdP grants.
///
/// RFC 8628 leaves the lifetime to the authorization server, and IdPs range
/// from five minutes to a day. Half an hour is long enough to walk to another
/// room and find a phone, and short enough that an abandoned flow -- whose
/// user code is the one thing a phisher needs (RFC 8628 section 5.4) -- does
/// not stay redeemable all day.
pub(crate) const DEVICE_LOGIN_MAX_SECS: u64 = 1800;

// ── Wire types ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct MeResponse {
    pub id: String,
    pub email: Option<String>,
    pub is_admin: bool,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// One of the current user's active sessions, as returned by `GET
/// /sessions`. `id` is an opaque row identifier for revocation via `DELETE
/// /sessions/{id}` -- never the session credential itself, which is hashed
/// at rest and cannot be recovered.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct SessionSummary {
    pub id: String,
    pub device_hash: String,
    pub ip: String,
    pub created_at: i64,
    pub last_active: i64,
}

/// Where the browser is sent back to after a successful login.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct LoginQuery {
    /// Path to return to after login.
    pub redirect: Option<String>,
}

/// What the IdP sends back to `/v1/auth/callback`.
#[derive(Debug, Serialize, Deserialize, Schema, QueryParams)]
pub struct CallbackQuery {
    pub state: Option<String>,
    pub code: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// What `POST /v1/auth/device` answers: what to show the user, and the handle
/// to poll with.
///
/// The IdP's device code is not here. Beam keeps it and polls the IdP itself,
/// so a client can only ever turn an approval into a Beam session -- never
/// into IdP tokens (ADR-0003's BFF property, kept for native clients).
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginStart {
    /// Opaque, single-flow secret the client polls `POST
    /// /v1/auth/device/token` with. Only its SHA-256 is stored.
    pub device_handle: String,
    /// The code the user enters at `verification_uri`.
    pub user_code: String,
    /// Where the user approves the sign-in, on any device with a browser.
    pub verification_uri: String,
    /// `verification_uri` with the code filled in, when the IdP offers one --
    /// suitable for a QR code.
    pub verification_uri_complete: Option<String>,
    /// How long the user has to approve, in seconds.
    pub expires_in_secs: u64,
    /// The minimum wait between polls, in seconds.
    pub interval_secs: u32,
}

/// The body of `POST /v1/auth/device/token`.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginPoll {
    /// The `device_handle` `POST /v1/auth/device` returned.
    pub device_handle: String,
}

/// Why a device login is still waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLoginWait {
    /// The user has not approved yet. Poll again after `interval_secs`.
    AuthorizationPending,
    /// The poll came too soon. The interval has grown; wait the new
    /// `interval_secs` before polling again.
    SlowDown,
}

/// A device login that has not finished yet.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginPending {
    pub status: DeviceLoginWait,
    /// The minimum wait before the next poll, in seconds.
    pub interval_secs: u32,
}

/// A device login the user approved: the session it minted.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginComplete {
    /// The `beam_session` credential. Present it as the `beam_session`
    /// cookie on every later request; it is the same opaque session a
    /// browser login mints, with the same idle and absolute expiry.
    pub session_token: String,
    /// The session's hard lifetime from now, in seconds.
    pub session_expires_in_secs: u64,
    /// Who signed in.
    pub user: MeResponse,
}

/// The two answers a device-login poll can give.
///
/// Two statuses rather than one body with a flag, so a generated client
/// distinguishes "keep waiting" from "signed in" by type.
#[derive(Reply)]
pub enum DeviceLoginPollReply {
    #[reply(status = 200, description = "The user approved; a session was minted")]
    SignedIn(DeviceLoginComplete),

    #[reply(
        status = 202,
        description = "The user has not approved yet; poll again"
    )]
    Pending(DeviceLoginPending),
}

/// What `/v1/sessions/{id}` captures.
#[derive(Debug, Schema, PathParams)]
pub struct SessionPath {
    /// Session id, from `GET /sessions`.
    pub id: String,
}

/// The cookies these endpoints read for themselves.
///
/// `beam_session` is read by `SessionAuth` everywhere else; `logout` takes it
/// here instead because it is deliberately callable without a valid session --
/// signing out of an already-expired session should succeed, not 401.
#[derive(Debug, Schema, CookieParams)]
pub struct AuthCookies {
    /// The CSRF state cookie set when the login round-trip began.
    pub beam_oidc_state: Option<String>,
    /// The session credential, when the caller holds one.
    pub beam_session: Option<String>,
}

// ── Response header groups ───────────────────────────────────────────────────

/// A `Set-Cookie` this operation writes.
///
/// Kynos has no per-handler cookie jar: `SetCookies` is an interceptor for a
/// fixed cookie, and a session credential is minted per request. A header group
/// is the sanctioned way to say it, and it puts `Set-Cookie` in the operation's
/// declared response headers -- which the Salvo implementation never did.
#[derive(Schema, HeaderParams)]
pub struct SetCookie {
    #[header(rename = "Set-Cookie")]
    set_cookie: String,
}

/// `Cache-Control: no-store` and `Pragma: no-cache` on a response that
/// carries a credential or a login secret.
///
/// RFC 6749 section 5.1 requires both on a token response, and RFC 8628
/// section 3.2 allows them on a device authorization response. The device
/// login's start (a `device_handle`) and its poll (a `session_token`, once
/// approved) are Beam's versions of those two, so an intermediary or the
/// client's HTTP cache must never keep a copy.
#[derive(Schema, HeaderParams)]
pub struct NoStore {
    #[header(rename = "Cache-Control")]
    cache_control: String,
    #[header(rename = "Pragma")]
    pragma: String,
}

impl NoStore {
    fn new() -> Self {
        Self {
            cache_control: "no-store".to_owned(),
            pragma: "no-cache".to_owned(),
        }
    }
}

/// A `Set-Cookie` value that could not be rendered as header text.
///
/// Its own type rather than a variant of any handler's error, because every
/// handler that writes a cookie needs it and none of them should widen to the
/// rest of another's statuses to get it. `oidc_logout` returned the login
/// solely to carry this, and so advertised a 400, a 403 and a 503 it could not
/// produce.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CookieEncodingError(String);

impl SetCookie {
    fn new(cookie: &Cookie) -> Result<Self, CookieEncodingError> {
        let encoded = cookie
            .encode()
            .ok_or_else(|| CookieEncodingError("could not encode session cookie".into()))?;
        let set_cookie = encoded
            .to_str()
            .map_err(|_| CookieEncodingError("session cookie is not valid header text".into()))?
            .to_owned();
        Ok(Self { set_cookie })
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────
//
// One type per operation shape, for the reason `api_error` sets out: Kynos
// derives an operation's responses from its return type, and carries one
// response per status titled from the first variant declaring it.

/// `GET /v1/auth/login`.
///
/// No 400 and no 403: the only input is a redirect path, which is sanitised
/// rather than rejected, and nothing here is authorised.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum LoginStartError {
    /// OIDC is not configured, or discovery failed. Distinct from a 500: the
    /// server is working, the identity provider is not reachable.
    #[error("{0}")]
    #[problem(
        status = 503,
        type = "https://beam.justinchung.net/reference/errors/#oidc-unavailable",
        title = "Login unavailable"
    )]
    OidcUnavailable(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `GET /v1/auth/callback`.
///
/// The two 400s share one declared response and `AttemptInvalid` is written
/// first, because it is the one a caller hits by reloading the callback URL --
/// which is the single most common way to reach this endpoint in error.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum LoginCallbackError {
    /// The round-trip cannot be matched to one this server started: the state
    /// cookie or parameter is missing, the two disagree, the pending record
    /// was already consumed or has expired, or the code is absent.
    ///
    /// One code rather than five, because a client acts on all of them
    /// identically -- start again from the app -- and enumerating exactly which
    /// half of a CSRF check failed tells an attacker more than it tells a user.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#login-attempt-invalid",
        title = "Login attempt is not valid"
    )]
    AttemptInvalid(String),

    /// The identity provider refused, or the token exchange did not verify.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#login-failed",
        title = "Login failed"
    )]
    LoginFailed(String),

    /// The identity verified; the local account is disabled (issue #85).
    #[error("{0}")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/#account-disabled",
        title = "Account is disabled"
    )]
    AccountDisabled(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `POST /v1/auth/device`.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum DeviceLoginStartError {
    /// The IdP's discovery document names no device authorization endpoint,
    /// so this deployment offers only the browser login. A client falls back
    /// to it rather than retrying.
    #[error("{0}")]
    #[problem(
        status = 501,
        type = "https://beam.justinchung.net/reference/errors/#device-login-unsupported",
        title = "Device login is not supported"
    )]
    Unsupported(String),

    #[error("{0}")]
    #[problem(
        status = 503,
        type = "https://beam.justinchung.net/reference/errors/#oidc-unavailable",
        title = "Login unavailable"
    )]
    OidcUnavailable(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `POST /v1/auth/device/token`.
///
/// `Invalid` is written first so it titles the 400: it is what a client
/// meets by polling a flow that already ended, the common mistake.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum DeviceLoginPollError {
    /// No open device login has this handle: it was never issued, or the
    /// flow already ended (signed in, denied, or expired and collected).
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#device-login-invalid",
        title = "Device login is not valid"
    )]
    Invalid(String),

    /// The IdP's answer did not verify.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#login-failed",
        title = "Login failed"
    )]
    LoginFailed(String),

    /// The user refused the sign-in at the IdP.
    #[error("{0}")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/#device-login-denied",
        title = "Device login was denied"
    )]
    Denied(String),

    /// The identity verified; the local account is disabled (issue #85).
    #[error("{0}")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/#account-disabled",
        title = "Account is disabled"
    )]
    AccountDisabled(String),

    /// The user did not approve in time. Start a new device login.
    #[error("{0}")]
    #[problem(
        status = 410,
        type = "https://beam.justinchung.net/reference/errors/#device-login-expired",
        title = "Device login expired"
    )]
    Expired(String),

    /// The IdP could not be reached or refused Beam itself. The flow is kept:
    /// polling again may succeed.
    #[error("{0}")]
    #[problem(
        status = 503,
        type = "https://beam.justinchung.net/reference/errors/#oidc-unavailable",
        title = "Login unavailable"
    )]
    OidcUnavailable(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `GET /v1/me`.
///
/// Keeps a 401 of its own rather than leaving it to `SessionAuth`, because it
/// means something the extractor's does not: the session verified, and the
/// account behind it has since been deleted.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum CurrentUserError {
    #[error("{0}")]
    #[problem(
        status = 401,
        type = "https://beam.justinchung.net/reference/errors/#account-removed",
        title = "Account no longer exists"
    )]
    AccountRemoved(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `DELETE /v1/sessions/{id}`.
///
/// Keeps its own 401 because the status carries meaning here that
/// `SessionAuth`'s does not: a session id that does not exist and one that
/// belongs to somebody else answer identically and deliberately, so a caller
/// cannot enumerate other people's sessions.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum SessionRevokeError {
    /// The `{id}` in the path is not a UUID.
    ///
    /// Its position titles nothing. Kynos titles a status from the first
    /// declaration it meets, and the `Path` extractor's 400 and `SessionAuth`'s
    /// 401 are both met before this enum, so the document reads "Bad Request"
    /// and "Unauthorized" for those statuses whatever order the variants take;
    /// order decides a title only for a status no extractor or authenticator
    /// declares.
    ///
    /// The operation already advertised a 400 it had no way to reach: every
    /// store error, the id parse included, was flattened into `Internal`, so a
    /// client's typo came back as "Beam broke" (issue #123). Its own code
    /// rather than the 401, because a malformed id and a session that is not
    /// there are different things for a caller to do something about.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-session-id",
        title = "Invalid session id"
    )]
    InvalidSessionId(String),

    #[error("{0}")]
    #[problem(
        status = 401,
        type = "https://beam.justinchung.net/reference/errors/#session-not-found",
        title = "Session not found"
    )]
    SessionNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

// A cookie that cannot be rendered is a server fault wherever it happens, so
// each of these is the same 500 reached through a different handler's type.

impl From<CookieEncodingError> for LoginStartError {
    fn from(e: CookieEncodingError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<CookieEncodingError> for LoginCallbackError {
    fn from(e: CookieEncodingError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<CookieEncodingError> for SessionRevokeError {
    fn from(e: CookieEncodingError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<CookieEncodingError> for InternalError {
    fn from(e: CookieEncodingError) -> Self {
        Self::Internal(e.to_string())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// The proxy-supplied headers the session record stores.
#[derive(Debug, Schema, HeaderParams)]
pub struct ClientHeaders {
    #[header(rename = "User-Agent")]
    pub user_agent: Option<String>,
    #[header(rename = "X-Forwarded-For")]
    pub x_forwarded_for: Option<String>,
    #[header(rename = "X-Real-IP")]
    pub x_real_ip: Option<String>,
}

fn build_cookie(name: &str, value: String, path: &str, secure: bool, max_age: Duration) -> Cookie {
    let cookie = Cookie::new(name.to_owned(), value)
        .path(path.to_owned())
        .http_only()
        .same_site(SameSite::Lax)
        .max_age(max_age);

    if secure { cookie.secure() } else { cookie }
}

/// A cookie that clears the one it names.
fn clearing_cookie(name: &str, path: &str) -> Cookie {
    Cookie::removal(name.to_owned()).path(path.to_owned())
}

/// Sanitizes a client-supplied post-login redirect target: must be a
/// same-origin-relative path (leading `/`, not `//...` or `/\...` -- both
/// of which some browsers treat as protocol-relative and would send the
/// user off-site). Anything else falls back to `/`.
fn sanitize_redirect_path(raw: Option<&str>) -> String {
    match raw {
        Some(path)
            if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") =>
        {
            path.to_owned()
        }
        _ => "/".to_owned(),
    }
}

impl ClientHeaders {
    /// The session record's view of the caller.
    fn context(&self) -> ClientContext {
        ClientContext {
            user_agent: self.user_agent.clone(),
            ip: client_ip(self.x_forwarded_for.as_deref(), self.x_real_ip.as_deref()),
        }
    }
}

/// The wire shape of a user, shared by `GET /v1/me` and a device login.
fn me_from(user: User) -> MeResponse {
    MeResponse {
        id: user.id.to_string(),
        email: user.email,
        is_admin: user.is_admin,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }
}

// ── Endpoints ────────────────────────────────────────────────────────────────

/// Begins an Authorization Code + PKCE flow and redirects the browser to
/// the IdP. `redirect` (query param) is where the callback sends the
/// browser back to on success; sanitized to a same-origin-relative path.
#[kynos::get("/auth/login", tag = Auth, operation_id = "oidcLogin")]
pub async fn oidc_login(
    Query(query): Query<LoginQuery>,
    Inject(oidc_client): Inject<Arc<dyn OidcClient>>,
    Inject(pending_auth_store): Inject<Arc<dyn PendingAuthStore>>,
    Inject(config): Inject<OidcRuntimeConfig>,
) -> Result<WithHeaders<Redirect<302>, SetCookie>, LoginStartError> {
    let redirect_path = sanitize_redirect_path(query.redirect.as_deref());

    let begin = oidc_client
        .begin_auth()
        .map_err(|e| LoginStartError::OidcUnavailable(format!("OIDC login unavailable: {e}")))?;

    pending_auth_store
        .create(
            &PendingAuth {
                state: begin.state.clone(),
                nonce: begin.nonce.clone(),
                pkce_verifier: begin.pkce_verifier.clone(),
                redirect_path: Some(redirect_path),
            },
            STATE_TTL_SECS,
        )
        .await
        .map_err(|e| LoginStartError::Internal(format!("Failed to start OIDC login: {e}")))?;

    let cookie = build_cookie(
        STATE_COOKIE,
        begin.state,
        "/v1/auth",
        config.cookie_secure,
        Duration::from_secs(STATE_TTL_SECS),
    );

    Ok(WithHeaders::new(
        Redirect::to(begin.auth_url),
        SetCookie::new(&cookie)?,
    ))
}

/// Completes the Authorization Code + PKCE exchange, JIT-provisions or
/// looks up the user, mints a session, and redirects back into the web app.
// Eight parameters, and none of them is an argument in the sense the lint
// means. Nothing calls this function: Kynos resolves each parameter from the
// request or the context, and the list *is* the operation's declared contract --
// three extractors and five injected dependencies. Collapsing them into a
// struct would hide the contract from the description without removing a single
// dependency. `expect` rather than `allow` so it reports itself if the
// signature ever shrinks below the threshold.
#[expect(
    clippy::too_many_arguments,
    reason = "each parameter is a declared extractor or injection, not a caller-supplied argument"
)]
#[kynos::get("/auth/callback", tag = Auth, operation_id = "oidcCallback")]
pub async fn oidc_callback(
    Query(query): Query<CallbackQuery>,
    Cookies(cookies): Cookies<AuthCookies>,
    Headers(headers): Headers<ClientHeaders>,
    Inject(oidc_client): Inject<Arc<dyn OidcClient>>,
    Inject(pending_auth_store): Inject<Arc<dyn PendingAuthStore>>,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
    Inject(user_repo): Inject<Arc<dyn UserRepository>>,
    Inject(config): Inject<OidcRuntimeConfig>,
) -> Result<WithHeaders<Redirect<302>, SetCookie>, LoginCallbackError> {
    if let Some(error) = query.error {
        let description = query.error_description.unwrap_or_default();
        return Err(LoginCallbackError::LoginFailed(format!(
            "IdP returned error: {error} {description}"
        )));
    }

    let state_cookie = cookies
        .beam_oidc_state
        .ok_or_else(|| LoginCallbackError::AttemptInvalid("Missing state cookie".into()))?;

    let query_state = query
        .state
        .ok_or_else(|| LoginCallbackError::AttemptInvalid("Missing state parameter".into()))?;

    if state_cookie != query_state {
        return Err(LoginCallbackError::AttemptInvalid(
            "State mismatch between cookie and callback".into(),
        ));
    }

    let pending = pending_auth_store
        .consume(&query_state)
        .await
        .map_err(|e| LoginCallbackError::Internal(e.to_string()))?
        .ok_or_else(|| {
            LoginCallbackError::AttemptInvalid(
                "Unknown, already-used, or expired login attempt".into(),
            )
        })?;

    let code = query
        .code
        .ok_or_else(|| LoginCallbackError::AttemptInvalid("Missing code parameter".into()))?;

    let identity = oidc_client
        .exchange_code(&code, &pending.pkce_verifier, &pending.nonce)
        .await
        .map_err(|e| match e {
            OidcError::NonceMismatch => {
                LoginCallbackError::LoginFailed("Nonce mismatch".to_owned())
            }
            other => LoginCallbackError::LoginFailed(format!("Login failed: {other}")),
        })?;

    let MintedSession {
        token,
        user: _,
        absolute_ttl_secs,
    } = complete_login(
        &identity,
        &headers.context(),
        user_repo.as_ref(),
        session_store.as_ref(),
        &config,
    )
    .await
    .map_err(|e| match e {
        e @ LoginError::AccountDisabled => LoginCallbackError::AccountDisabled(e.to_string()),
        LoginError::Internal(message) => LoginCallbackError::Internal(message),
    })?;

    let cookie = build_cookie(
        SESSION_COOKIE,
        token,
        "/",
        config.cookie_secure,
        Duration::from_secs(absolute_ttl_secs),
    );

    let redirect_path = pending.redirect_path.unwrap_or_else(|| "/".to_owned());

    Ok(WithHeaders::new(
        Redirect::to(format!("{}{}", config.web_url, redirect_path)),
        SetCookie::new(&cookie)?,
    ))
}

/// Starts a device login (RFC 8628) for a client that has no browser.
///
/// Asks the IdP for a device code, keeps it, and hands the client an opaque
/// handle plus the user code to display. The user approves on any other
/// device; the client polls `POST /v1/auth/device/token` meanwhile.
///
/// Answers 501 when the IdP does not offer the grant: the deployment still
/// signs browsers in, and a client with a browser falls back to that.
#[kynos::post("/auth/device", tag = Auth, operation_id = "startDeviceLogin")]
pub async fn start_device_login(
    Inject(oidc_client): Inject<Arc<dyn OidcClient>>,
    Inject(device_auth_store): Inject<Arc<dyn DeviceAuthStore>>,
) -> Result<WithHeaders<Json<DeviceLoginStart>, NoStore>, DeviceLoginStartError> {
    let started = oidc_client.begin_device_auth().await.map_err(|e| match e {
        OidcError::DeviceFlowUnsupported => DeviceLoginStartError::Unsupported(e.to_string()),
        other => DeviceLoginStartError::OidcUnavailable(format!("OIDC login unavailable: {other}")),
    })?;

    let handle = generate_handle();
    let expires_in_secs = started.expires_in_secs.min(DEVICE_LOGIN_MAX_SECS);
    // An IdP that asks for no interval at all would switch the pacing off;
    // one second is the floor.
    let interval_secs = u32::try_from(started.interval_secs.max(1)).unwrap_or(u32::MAX);

    device_auth_store
        .create(&NewDeviceAuth {
            handle_hash: hash_handle(&handle),
            device_code: started.device_code,
            user_code: started.user_code.clone(),
            verification_uri: started.verification_uri.clone(),
            verification_uri_complete: started.verification_uri_complete.clone(),
            interval_secs,
            expires_in_secs,
        })
        .await
        .map_err(|e| {
            DeviceLoginStartError::Internal(format!("Failed to start device login: {e}"))
        })?;

    Ok(WithHeaders::new(
        Json(DeviceLoginStart {
            device_handle: handle,
            user_code: started.user_code,
            verification_uri: started.verification_uri,
            verification_uri_complete: started.verification_uri_complete,
            expires_in_secs,
            interval_secs,
        }),
        NoStore::new(),
    ))
}

/// Polls a device login once.
///
/// Each call reaches the IdP at most once, and not at all when it comes
/// sooner than the flow's interval allows -- that answers `slow_down` and
/// grows the interval, as RFC 8628 section 3.5 has the IdP do. On approval
/// the session is minted exactly as the browser callback mints it (same
/// admin claim, JIT provisioning, disabled gate, and expiry) and its opaque
/// value returned in the body, to be presented as the `beam_session` cookie
/// on every later request.
//
// Cookie, not `Authorization: Bearer`: Kynos 0.3's `Auth<S>` binds one scheme
// per operation and has no way to declare "cookie or bearer" (an OpenAPI
// security requirement list of alternatives), so accepting a bearer token
// would mean either a second, undescribed authenticator or re-declaring every
// secured operation.
//
// Upstream gap in kynos, NOT YET FILED -- to be filed on getkono/kynos by the
// maintainer (the run that wrote this could not file there), with this title
// and ask, also recorded in ADR-0017 (D151-3):
//
//   "Auth<S> cannot declare alternative security schemes (OpenAPI any-of
//   security requirements)": an operation that accepts a cookie session *or*
//   an `Authorization: Bearer` token cannot be described; `Auth<S>` binds one
//   scheme and emits a single security requirement. Asked for: an any-of
//   form of `Auth` that emits `security: [{cookie: []}, {bearer: []}]` and
//   authenticates with whichever credential the request carries.
//
// Replace this block with the issue link once filed. Until a release closes
// it, the one described scheme is the one every client uses.
#[kynos::post("/auth/device/token", tag = Auth, operation_id = "pollDeviceLogin")]
pub async fn poll_device_login(
    Headers(headers): Headers<ClientHeaders>,
    Inject(oidc_client): Inject<Arc<dyn OidcClient>>,
    Inject(device_auth_store): Inject<Arc<dyn DeviceAuthStore>>,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
    Inject(user_repo): Inject<Arc<dyn UserRepository>>,
    Inject(config): Inject<OidcRuntimeConfig>,
    Json(body): Json<DeviceLoginPoll>,
) -> Result<WithHeaders<DeviceLoginPollReply, NoStore>, DeviceLoginPollError> {
    let internal = |e: beam_auth::utils::device_auth_store::DeviceAuthError| {
        DeviceLoginPollError::Internal(e.to_string())
    };
    let handle_hash = hash_handle(&body.device_handle);

    let auth = match device_auth_store
        .claim_poll(&handle_hash)
        .await
        .map_err(internal)?
    {
        Claim::NotFound => {
            return Err(DeviceLoginPollError::Invalid(
                "Unknown or already-finished device login".into(),
            ));
        }
        Claim::Expired => {
            device_auth_store
                .consume(&handle_hash)
                .await
                .map_err(internal)?;
            return Err(DeviceLoginPollError::Expired(
                "The device login expired before it was approved".into(),
            ));
        }
        Claim::TooEarly { interval_secs } => {
            // The IdP is not asked: answering for it is the point.
            let interval_secs = device_auth_store
                .bump_interval(&handle_hash)
                .await
                .map_err(internal)?
                .unwrap_or(interval_secs);
            return Ok(WithHeaders::new(
                DeviceLoginPollReply::Pending(DeviceLoginPending {
                    status: DeviceLoginWait::SlowDown,
                    interval_secs,
                }),
                NoStore::new(),
            ));
        }
        Claim::Claimed(auth) => auth,
    };

    let identity = match oidc_client.poll_device_token(&auth.device_code).await {
        Ok(DevicePoll::Pending) => {
            return Ok(WithHeaders::new(
                DeviceLoginPollReply::Pending(DeviceLoginPending {
                    status: DeviceLoginWait::AuthorizationPending,
                    interval_secs: auth.interval_secs,
                }),
                NoStore::new(),
            ));
        }
        Ok(DevicePoll::SlowDown) => {
            let interval_secs = device_auth_store
                .bump_interval(&handle_hash)
                .await
                .map_err(internal)?
                .unwrap_or(auth.interval_secs);
            return Ok(WithHeaders::new(
                DeviceLoginPollReply::Pending(DeviceLoginPending {
                    status: DeviceLoginWait::SlowDown,
                    interval_secs,
                }),
                NoStore::new(),
            ));
        }
        Ok(DevicePoll::Denied) => {
            device_auth_store
                .consume(&handle_hash)
                .await
                .map_err(internal)?;
            return Err(DeviceLoginPollError::Denied(
                "The sign-in was refused at the identity provider".into(),
            ));
        }
        Ok(DevicePoll::Expired) => {
            device_auth_store
                .consume(&handle_hash)
                .await
                .map_err(internal)?;
            return Err(DeviceLoginPollError::Expired(
                "The device login expired before it was approved".into(),
            ));
        }
        Ok(DevicePoll::Complete(identity)) => identity,
        // The IdP could not be reached, or Beam is misconfigured against it:
        // nothing about this flow is wrong, so it is kept for the next poll.
        Err(
            e @ (OidcError::Discovery(_)
            | OidcError::Exchange(_)
            | OidcError::DeviceFlowUnsupported),
        ) => {
            return Err(DeviceLoginPollError::OidcUnavailable(format!(
                "OIDC login unavailable: {e}"
            )));
        }
        // An answer that did not verify will not verify on a retry.
        Err(
            e @ (OidcError::MissingIdToken
            | OidcError::ClaimsVerification(_)
            | OidcError::NonceMismatch),
        ) => {
            device_auth_store
                .consume(&handle_hash)
                .await
                .map_err(internal)?;
            return Err(DeviceLoginPollError::LoginFailed(format!(
                "Login failed: {e}"
            )));
        }
    };

    // Ending the flow before minting is what makes an approval single-use: of
    // two polls that both saw it approved, only the one that removes the row
    // signs in.
    if device_auth_store
        .consume(&handle_hash)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(DeviceLoginPollError::Invalid(
            "Unknown or already-finished device login".into(),
        ));
    }

    let MintedSession {
        token,
        user,
        absolute_ttl_secs,
    } = complete_login(
        &identity,
        &headers.context(),
        user_repo.as_ref(),
        session_store.as_ref(),
        &config,
    )
    .await
    .map_err(|e| match e {
        e @ LoginError::AccountDisabled => DeviceLoginPollError::AccountDisabled(e.to_string()),
        LoginError::Internal(message) => DeviceLoginPollError::Internal(message),
    })?;

    Ok(WithHeaders::new(
        DeviceLoginPollReply::SignedIn(DeviceLoginComplete {
            session_token: token,
            session_expires_in_secs: absolute_ttl_secs,
            user: me_from(user),
        }),
        NoStore::new(),
    ))
}

/// Returns the currently authenticated user (via the `beam_session` cookie).
#[kynos::get("/me", tag = Auth, operation_id = "getCurrentUser")]
pub async fn oidc_me(
    auth: SessionAuth,
    Inject(user_repo): Inject<Arc<dyn UserRepository>>,
) -> Result<Json<MeResponse>, CurrentUserError> {
    let user_uuid =
        Uuid::parse_str(&auth.0.user_id).map_err(|e| CurrentUserError::Internal(e.to_string()))?;

    let user = user_repo
        .find_by_id(user_uuid)
        .await
        .map_err(|e| CurrentUserError::Internal(e.to_string()))?
        .ok_or_else(|| CurrentUserError::AccountRemoved("User no longer exists".into()))?;

    Ok(Json(me_from(user)))
}

/// Logs out the current session (deletes it and clears the cookie).
///
/// Deliberately not `SessionAuth`-gated: signing out of a session that has
/// already expired should succeed rather than answer 401, so the cookie is read
/// directly and a miss is a no-op.
#[kynos::post("/logout", tag = Auth, operation_id = "logout")]
pub async fn oidc_logout(
    Cookies(cookies): Cookies<AuthCookies>,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
) -> Result<WithHeaders<NoContent, SetCookie>, InternalError> {
    if let Some(token) = cookies.beam_session {
        let _ = session_store.delete(&token).await;
    }

    Ok(WithHeaders::new(
        NoContent,
        SetCookie::new(&clearing_cookie(SESSION_COOKIE, "/"))?,
    ))
}

/// Logs out every active session for the current user.
#[kynos::post("/logout-all", tag = Auth, operation_id = "logoutAll")]
pub async fn oidc_logout_all(
    auth: SessionAuth,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
) -> Result<WithHeaders<NoContent, SetCookie>, InternalError> {
    session_store
        .delete_all_for_user(&auth.0.user_id)
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;

    Ok(WithHeaders::new(
        NoContent,
        SetCookie::new(&clearing_cookie(SESSION_COOKIE, "/"))?,
    ))
}

/// Lists every active session for the current user.
#[kynos::get("/sessions", tag = Auth, operation_id = "listSessions")]
pub async fn oidc_list_sessions(
    auth: SessionAuth,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
) -> Result<Json<Vec<SessionSummary>>, InternalError> {
    let sessions = session_store
        .list_for_user(&auth.0.user_id)
        .await
        .map_err(|e| InternalError::Internal(e.to_string()))?;

    Ok(Json(
        sessions
            .into_iter()
            .map(|(id, data)| SessionSummary {
                id,
                device_hash: data.device_hash,
                ip: data.ip,
                created_at: data.created_at,
                last_active: data.last_active,
            })
            .collect(),
    ))
}

/// Revokes a specific session by its listing id, scoped to the current user
/// (returns 401 for a session that doesn't exist or belongs to someone
/// else, never distinguishing the two).
#[kynos::delete("/sessions/{id}", tag = Auth, operation_id = "deleteSession")]
pub async fn oidc_delete_session(
    auth: SessionAuth,
    Path(path): Path<SessionPath>,
    Cookies(cookies): Cookies<AuthCookies>,
    Inject(session_store): Inject<Arc<dyn SessionStore>>,
) -> Result<SessionRevoked, SessionRevokeError> {
    let deleted = session_store
        .delete_by_id(&path.id, &auth.0.user_id)
        .await
        .map_err(|e| match e {
            // The store parses the path id before it queries, so this is the
            // caller's typo rather than a fault. The user id cannot reach here
            // malformed -- it comes from an authenticated session.
            SessionError::InvalidId(_) => SessionRevokeError::InvalidSessionId(format!(
                "{} is not a valid session id",
                path.id
            )),
            other => SessionRevokeError::Internal(other.to_string()),
        })?;

    if !deleted {
        return Err(SessionRevokeError::SessionNotFound(
            "Session not found".to_owned(),
        ));
    }

    // Revoking the session the caller is currently using should also clear
    // their cookie, rather than leaving a dead cookie around.
    //
    // Whether that happened is decided by re-reading the caller's own token:
    // this used to compare the request cookie to a value *derived from that
    // same cookie* and so was always equal -- revoking any other device signed
    // the caller out of the one they were holding.
    let still_valid = match cookies.beam_session {
        Some(token) => session_store
            .get(&token)
            .await
            .map_err(|e| SessionRevokeError::Internal(e.to_string()))?
            .is_some(),
        None => false,
    };

    if still_valid {
        Ok(SessionRevoked::Kept(NoContent))
    } else {
        let cleared = SetCookie::new(&clearing_cookie(SESSION_COOKIE, "/"))?;
        Ok(SessionRevoked::SignedOut(WithHeaders::new(
            NoContent, cleared,
        )))
    }
}

/// Whether revoking a session also signed the caller out of this device.
///
/// Both arms are 204, which `Reply` forbids -- it keys variants by status --
/// so this is a hand-written `IntoResponse`/`Responses` pair. The two differ
/// only in whether `Set-Cookie` is present, which is a header, not a status.
pub enum SessionRevoked {
    /// The caller's own session survived; nothing to clear.
    Kept(NoContent),
    /// The caller revoked the session they were holding.
    SignedOut(WithHeaders<NoContent, SetCookie>),
}

impl kynos::response::IntoResponse for SessionRevoked {
    fn into_response(self) -> kynos::http::Response {
        match self {
            Self::Kept(inner) => inner.into_response(),
            Self::SignedOut(inner) => inner.into_response(),
        }
    }
}

impl kynos::response::Responses for SessionRevoked {
    /// The optional-header shape: 204 either way, with `Set-Cookie` marked as
    /// present only sometimes.
    fn responses(registry: &mut kynos::schema::registry::Registry) -> kynos::openapi::Responses {
        <WithHeaders<NoContent, SetCookie> as kynos::response::Responses>::responses(registry)
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;

#[cfg(test)]
#[path = "device_login_tests.rs"]
mod device_login_tests;

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn a_relative_path_is_kept() {
        assert_eq!(sanitize_redirect_path(Some("/library/42")), "/library/42");
    }

    /// Both of these are read as protocol-relative by some browsers, which
    /// would send the user to another origin carrying their session.
    #[test]
    fn a_protocol_relative_path_falls_back_to_root() {
        for hostile in ["//evil.example.com", "/\\evil.example.com"] {
            assert_eq!(sanitize_redirect_path(Some(hostile)), "/");
        }
    }

    #[test]
    fn an_absolute_url_falls_back_to_root() {
        assert_eq!(
            sanitize_redirect_path(Some("https://evil.example.com")),
            "/"
        );
        assert_eq!(sanitize_redirect_path(None), "/");
    }
}
