//! OIDC BFF endpoints (see ADR-0003): `login`/`callback` drive the
//! Authorization Code + PKCE round-trip, `me`/`logout`/`logout-all`/
//! `sessions`/`sessions/{id}` operate on the resulting `beam_session`
//! cookie -- the sole credential beam-server issues. `login`/`callback` are
//! mounted under `/v1/auth/*` and the rest at the top level (`/v1/me`,
//! `/v1/logout`, ...).
//!
//! The browser never sees an IdP token; `beam_session` is the only
//! credential it holds, set as an httpOnly, `SameSite=Lax` cookie.
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

use beam_auth::utils::admin_claim::admin_claim_matches;
use beam_auth::utils::models::CreateUser;
use beam_auth::utils::oidc::{OidcClient, OidcError};
use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::pending_auth_store::{PendingAuth, PendingAuthStore};
use beam_auth::utils::repository::UserRepository;
use beam_auth::utils::session_store::{SessionData, SessionStore};
use chrono::Utc;
use kynos::prelude::*;
use kynos::response::cookie::{Cookie, SameSite};
use kynos::response::headers::WithHeaders;
use kynos::response::status::{NoContent, Redirect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::routes::api_error::{SESSION_COOKIE, SessionAuth};
use crate::routes::tags::Auth;

const STATE_COOKIE: &str = "beam_oidc_state";
const STATE_TTL_SECS: u64 = 600; // 10 minutes to complete the round trip

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

impl SetCookie {
    fn new(cookie: &Cookie) -> Result<Self, AuthError> {
        let encoded = cookie
            .encode()
            .ok_or_else(|| AuthError::Internal("could not encode session cookie".into()))?;
        let set_cookie = encoded
            .to_str()
            .map_err(|_| AuthError::Internal("session cookie is not valid header text".into()))?
            .to_owned();
        Ok(Self { set_cookie })
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// What the login round-trip can answer with.
///
/// 401 is absent: the operations that need a session take `SessionAuth`, which
/// contributes it. `SessionNotFound` is the one exception and lives in
/// [`SessionActionError`] -- see the note there.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum AuthError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/bad-request",
        title = "Bad request"
    )]
    BadRequest(String),

    /// The identity is valid but the local account is disabled (issue #85).
    #[error("{0}")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/forbidden",
        title = "Forbidden"
    )]
    Forbidden(String),

    /// OIDC is not configured, or discovery failed. Distinct from a 500: the
    /// server is working, the identity provider is not reachable.
    #[error("{0}")]
    #[problem(
        status = 503,
        type = "https://beam.justinchung.net/reference/errors/oidc-unavailable",
        title = "Login unavailable"
    )]
    Unavailable(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// Revoking a session by id.
///
/// Keeps its own 401 because the status carries meaning here that
/// `SessionAuth`'s does not: a session id that does not exist and one that
/// belongs to somebody else answer identically and deliberately, so a caller
/// cannot enumerate other people's sessions.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum SessionActionError {
    #[error("{0}")]
    #[problem(
        status = 401,
        type = "https://beam.justinchung.net/reference/errors/unauthorized",
        title = "Unauthorized"
    )]
    Unauthorized(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn device_hash(user_agent: Option<&str>) -> String {
    // `beam_auth`'s encoder, not a local one. Session rows are looked up by
    // equality against this string, and `sha2` 0.11 returns a
    // `hybrid_array::Array` with no `LowerHex`, so the `{:x}` this used to be
    // stopped compiling. A second implementation here would be a second thing
    // that has to keep producing identical bytes.
    beam_auth::utils::hex::encode_lower(&Sha256::digest(user_agent.unwrap_or("").as_bytes()))
}

/// The client address, as the deployment's proxy reports it.
fn client_ip(forwarded_for: Option<&str>, real_ip: Option<&str>) -> String {
    if let Some(first) = forwarded_for.and_then(|value| value.split(',').next()) {
        let first = first.trim();
        if !first.is_empty() {
            return first.to_owned();
        }
    }
    real_ip.map_or_else(|| "unknown".to_owned(), str::to_owned)
}

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

/// Picks a display name when the IdP doesn't release a `name` claim: the
/// local part of the email if one is available, else a subject-derived
/// placeholder. Real IdPs (including Dex) send `name`, so this is a rare
/// fallback, not the common case.
fn derive_display_name(name: Option<&str>, email: Option<&str>, subject: &str) -> String {
    if let Some(name) = name
        && !name.is_empty()
    {
        return name.to_owned();
    }
    if let Some(local_part) = email.and_then(|e| e.split('@').next())
        && !local_part.is_empty()
    {
        return local_part.to_owned();
    }
    format!("user-{subject}")
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
) -> Result<WithHeaders<Redirect<302>, SetCookie>, AuthError> {
    let redirect_path = sanitize_redirect_path(query.redirect.as_deref());

    let begin = oidc_client
        .begin_auth()
        .map_err(|e| AuthError::Unavailable(format!("OIDC login unavailable: {e}")))?;

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
        .map_err(|e| AuthError::Internal(format!("Failed to start OIDC login: {e}")))?;

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
) -> Result<WithHeaders<Redirect<302>, SetCookie>, AuthError> {
    if let Some(error) = query.error {
        let description = query.error_description.unwrap_or_default();
        return Err(AuthError::BadRequest(format!(
            "IdP returned error: {error} {description}"
        )));
    }

    let state_cookie = cookies
        .beam_oidc_state
        .ok_or_else(|| AuthError::BadRequest("Missing state cookie".into()))?;

    let query_state = query
        .state
        .ok_or_else(|| AuthError::BadRequest("Missing state parameter".into()))?;

    if state_cookie != query_state {
        return Err(AuthError::BadRequest(
            "State mismatch between cookie and callback".into(),
        ));
    }

    let pending = pending_auth_store
        .consume(&query_state)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
        .ok_or_else(|| {
            AuthError::BadRequest("Unknown, already-used, or expired login attempt".into())
        })?;

    let code = query
        .code
        .ok_or_else(|| AuthError::BadRequest("Missing code parameter".into()))?;

    let identity = oidc_client
        .exchange_code(&code, &pending.pkce_verifier, &pending.nonce)
        .await
        .map_err(|e| match e {
            OidcError::NonceMismatch => AuthError::BadRequest("Nonce mismatch".to_owned()),
            other => AuthError::BadRequest(format!("Login failed: {other}")),
        })?;

    // Admin is derived solely from a configured ID-token claim asserted by the
    // IdP (issue #85): the IdP is the single authority. Recomputed on every
    // login below, so it both grants and revokes -- and with no admin claim
    // configured, `false` here demotes any previously-admin user at next login.
    let is_admin = match config.admin_claim.as_deref() {
        Some(claim_name) => {
            admin_claim_matches(&identity.claims, claim_name, config.admin_value.as_deref())
        }
        None => false,
    };
    let display_name = derive_display_name(
        identity.name.as_deref(),
        identity.email.as_deref(),
        &identity.subject,
    );

    let user = match user_repo
        .find_by_oidc_identity(&identity.issuer, &identity.subject)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
    {
        Some(existing) => {
            // A disabled account is blocked at the door: no session is minted
            // and no profile/admin fields are touched (issue #85). Only an
            // already-provisioned account can be disabled -- JIT-provisioned
            // new users below are always created enabled.
            if existing.disabled {
                return Err(AuthError::Forbidden(
                    "This account has been disabled. Contact an administrator.".to_owned(),
                ));
            }
            if existing.is_admin != is_admin {
                user_repo
                    .set_admin(existing.id, is_admin)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
            }
            if existing.display_name != display_name || existing.avatar_url != identity.picture {
                user_repo
                    .update_oidc_profile(existing.id, display_name, identity.picture.clone())
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
            }
            existing
        }
        None => user_repo
            .create(CreateUser {
                oidc_issuer: identity.issuer.clone(),
                oidc_subject: identity.subject.clone(),
                email: identity.email.clone(),
                display_name,
                avatar_url: identity.picture.clone(),
                is_admin,
            })
            .await
            .map_err(|e| AuthError::Internal(format!("Failed to provision user: {e}")))?,
    };

    let idle_ttl_secs = config.idle_ttl_secs();
    let absolute_ttl_secs = config.absolute_ttl_secs();

    let session_data = SessionData {
        user_id: user.id.to_string(),
        device_hash: device_hash(headers.user_agent.as_deref()),
        ip: client_ip(
            headers.x_forwarded_for.as_deref(),
            headers.x_real_ip.as_deref(),
        ),
        created_at: Utc::now().timestamp(),
        last_active: Utc::now().timestamp(),
    };

    let token = session_store
        .create(&session_data, idle_ttl_secs, absolute_ttl_secs)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?;

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

/// Returns the currently authenticated user (via the `beam_session` cookie).
#[kynos::get("/me", tag = Auth, operation_id = "getCurrentUser")]
pub async fn oidc_me(
    auth: SessionAuth,
    Inject(user_repo): Inject<Arc<dyn UserRepository>>,
) -> Result<Json<MeResponse>, SessionActionError> {
    let user_uuid = Uuid::parse_str(&auth.0.user_id)
        .map_err(|e| SessionActionError::Internal(e.to_string()))?;

    let user = user_repo
        .find_by_id(user_uuid)
        .await
        .map_err(|e| SessionActionError::Internal(e.to_string()))?
        .ok_or_else(|| SessionActionError::Unauthorized("User no longer exists".into()))?;

    Ok(Json(MeResponse {
        id: user.id.to_string(),
        email: user.email,
        is_admin: user.is_admin,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }))
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
) -> Result<WithHeaders<NoContent, SetCookie>, AuthError> {
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
) -> Result<WithHeaders<NoContent, SetCookie>, AuthError> {
    session_store
        .delete_all_for_user(&auth.0.user_id)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?;

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
) -> Result<Json<Vec<SessionSummary>>, SessionActionError> {
    let sessions = session_store
        .list_for_user(&auth.0.user_id)
        .await
        .map_err(|e| SessionActionError::Internal(e.to_string()))?;

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
) -> Result<SessionRevoked, SessionActionError> {
    let deleted = session_store
        .delete_by_id(&path.id, &auth.0.user_id)
        .await
        .map_err(|e| SessionActionError::Internal(e.to_string()))?;

    if !deleted {
        return Err(SessionActionError::Unauthorized(
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
            .map_err(|e| SessionActionError::Internal(e.to_string()))?
            .is_some(),
        None => false,
    };

    if still_valid {
        Ok(SessionRevoked::Kept(NoContent))
    } else {
        let cleared = SetCookie::new(&clearing_cookie(SESSION_COOKIE, "/"))
            .map_err(|_| SessionActionError::Internal("could not clear session cookie".into()))?;
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

    #[test]
    fn the_forwarded_chain_yields_its_first_entry() {
        assert_eq!(
            client_ip(Some("203.0.113.7, 70.41.3.18"), Some("10.0.0.1")),
            "203.0.113.7"
        );
    }

    #[test]
    fn the_real_ip_header_is_the_fallback() {
        assert_eq!(client_ip(None, Some("10.0.0.1")), "10.0.0.1");
        assert_eq!(client_ip(None, None), "unknown");
    }

    #[test]
    fn a_name_claim_wins_over_the_email_local_part() {
        assert_eq!(
            derive_display_name(Some("Ada Lovelace"), Some("ada@example.com"), "sub"),
            "Ada Lovelace"
        );
    }

    #[test]
    fn an_absent_name_falls_back_to_the_email_local_part_then_the_subject() {
        assert_eq!(
            derive_display_name(None, Some("ada@example.com"), "sub"),
            "ada"
        );
        assert_eq!(derive_display_name(Some(""), None, "sub"), "user-sub");
        assert_eq!(derive_display_name(None, None, "sub"), "user-sub");
    }
}
