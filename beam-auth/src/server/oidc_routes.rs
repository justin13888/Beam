//! OIDC BFF endpoints (see ADR-0003): `login`/`callback` drive the
//! Authorization Code + PKCE round-trip, `me`/`logout`/`logout-all`/
//! `sessions`/`sessions/{id}` operate on the resulting `beam_session`
//! cookie -- the sole credential beam-server now issues. beam-server mounts
//! `login`/`callback` under `/v1/auth/*` and the rest at the top level
//! (`/v1/me`, `/v1/logout`, ...), matching the final ratified shape now that
//! the legacy password endpoints are gone.
//!
//! The browser never sees an IdP token; `beam_session` is the only
//! credential it holds, set as an httpOnly, `SameSite=Lax` cookie.

use async_trait::async_trait;
use chrono::Utc;
use salvo::http::cookie::{Cookie, SameSite, time::Duration as CookieDuration};
use salvo::oapi::{ToResponses, ToSchema};
use salvo::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use crate::utils::admin_claim::admin_claim_matches;
use crate::utils::models::CreateUser;
use crate::utils::oidc::{OidcClient, OidcError};
use crate::utils::pending_auth_store::{PendingAuth, PendingAuthStore};
use crate::utils::repository::UserRepository;
use crate::utils::session_store::{SessionData, SessionStore, get_and_touch};

const STATE_COOKIE: &str = "beam_oidc_state";
const SESSION_COOKIE: &str = "beam_session";
const STATE_TTL_SECS: u64 = 600; // 10 minutes to complete the round trip

/// Runtime configuration the OIDC routes need beyond what a single service
/// trait naturally carries -- injected into the depot alongside the
/// `Arc<dyn ...>` services.
#[derive(Debug, Clone)]
pub struct OidcRuntimeConfig {
    /// Base URL of the web client; the callback redirects here on success.
    pub web_url: String,
    /// Whether to mark cookies `Secure` (derived from the deployment's
    /// scheme; `false` only makes sense for plain-HTTP local dev).
    pub cookie_secure: bool,
    /// Name of the ID-token claim that grants admin (see `admin_claim`).
    /// `None` -> nobody is granted admin at login, and any existing admin is
    /// demoted at their next login (issue #85).
    pub admin_claim: Option<String>,
    /// Expected value for `admin_claim`. `None` -> the claim must assert
    /// boolean `true`; `Some(v)` -> the claim must equal `v` or (if an array)
    /// contain `v`.
    pub admin_value: Option<String>,
    pub session_idle_days: u64,
    pub session_max_days: u64,
}

fn device_hash_from_request(req: &Request) -> String {
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    format!("{:x}", Sha256::digest(user_agent.as_bytes()))
}

fn extract_client_ip(req: &Request) -> String {
    if let Some(forwarded_for) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        && let Some(first) = forwarded_for.split(',').next()
    {
        return first.trim().to_string();
    }
    if let Some(real_ip) = req.headers().get("x-real-ip").and_then(|v| v.to_str().ok()) {
        return real_ip.to_string();
    }
    "unknown".to_string()
}

fn build_cookie(
    name: &'static str,
    value: String,
    path: &'static str,
    secure: bool,
    max_age: CookieDuration,
) -> Cookie<'static> {
    Cookie::build((name, value))
        .path(path)
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(max_age)
        .build()
}

/// Sanitizes a client-supplied post-login redirect target: must be a
/// same-origin-relative path (leading `/`, not `//...` or `/\...` --  both
/// of which some browsers treat as protocol-relative and would send the
/// user off-site). Anything else falls back to `/`.
fn sanitize_redirect_path(raw: Option<&str>) -> String {
    match raw {
        Some(path)
            if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") =>
        {
            path.to_string()
        }
        _ => "/".to_string(),
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
        return name.to_string();
    }
    if let Some(local_part) = email.and_then(|e| e.split('@').next())
        && !local_part.is_empty()
    {
        return local_part.to_string();
    }
    format!("user-{subject}")
}

#[derive(Debug, Serialize, ToSchema)]
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
#[derive(Debug, Serialize, ToSchema)]
pub struct SessionSummary {
    pub id: String,
    pub device_hash: String,
    pub ip: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(ToResponses)]
pub enum OidcCallbackError {
    /// The state/nonce/PKCE round-trip failed verification.
    #[salvo(response(status_code = 400))]
    BadRequest(String),
    /// The identity is valid but the local account is disabled (issue #85).
    #[salvo(response(status_code = 403))]
    Forbidden(String),
    /// Internal server error
    #[salvo(response(status_code = 500))]
    InternalError(String),
}

#[async_trait]
impl Writer for OidcCallbackError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::BadRequest(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain(msg));
            }
            Self::Forbidden(msg) => {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Text::Plain(msg));
            }
            Self::InternalError(msg) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Text::Plain(msg));
            }
        }
    }
}

#[derive(ToResponses)]
pub enum OidcAuthError {
    /// Missing or invalid session
    #[salvo(response(status_code = 401))]
    Unauthorized(String),
    /// Internal server error
    #[salvo(response(status_code = 500))]
    InternalError(String),
}

#[async_trait]
impl Writer for OidcAuthError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::InternalError(msg) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Text::Plain(msg));
            }
        }
    }
}

/// Marker for a dependency the router wiring failed to inject; converts
/// into a 500 for whichever error type the handler returns.
struct MissingDependency;

impl From<MissingDependency> for OidcAuthError {
    fn from(_: MissingDependency) -> Self {
        Self::InternalError("Server state unavailable".to_string())
    }
}

impl From<MissingDependency> for OidcCallbackError {
    fn from(_: MissingDependency) -> Self {
        Self::InternalError("Server state unavailable".to_string())
    }
}

/// Fetches an injected dependency from the depot. Every `T` used here is
/// wired in by the host's router setup, so a miss is a router wiring bug --
/// surfaced as a 500 rather than a handler panic.
fn obtain_dep<T: Send + Sync + 'static>(depot: &Depot) -> Result<&T, MissingDependency> {
    depot.obtain::<T>().map_err(|_| {
        tracing::error!(
            dependency = std::any::type_name::<T>(),
            "dependency missing from depot -- router wiring bug"
        );
        MissingDependency
    })
}

/// Resolves the current user from the `beam_session` cookie, sliding the
/// idle expiry forward (throttled via [`get_and_touch`]).
async fn require_web_session(
    req: &Request,
    depot: &Depot,
) -> Result<(String, String), OidcAuthError> {
    let session_store = obtain_dep::<Arc<dyn SessionStore>>(depot)?;
    let config = obtain_dep::<OidcRuntimeConfig>(depot)?;

    let token = req
        .cookie(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| OidcAuthError::Unauthorized("Missing session cookie".into()))?;

    let idle_ttl_secs = config.session_idle_days * 24 * 60 * 60;
    let session = get_and_touch(session_store.as_ref(), &token, idle_ttl_secs)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?
        .ok_or_else(|| OidcAuthError::Unauthorized("Invalid or expired session".into()))?;

    Ok((session.user_id, token))
}

/// Begins an Authorization Code + PKCE flow and redirects the browser to
/// the IdP. `redirect` (query param) is where the callback sends the
/// browser back to on success; sanitized to a same-origin-relative path.
#[endpoint(
    tags("auth"),
    parameters(("redirect" = Option<String>, Query, description = "Path to return to after login")),
)]
pub async fn oidc_login(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let deps = (
        obtain_dep::<Arc<dyn OidcClient>>(depot),
        obtain_dep::<Arc<dyn PendingAuthStore>>(depot),
        obtain_dep::<OidcRuntimeConfig>(depot),
    );
    let (Ok(oidc_client), Ok(pending_auth_store), Ok(config)) = deps else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Plain("Server state unavailable"));
        return;
    };
    let (oidc_client, pending_auth_store, config) = (
        oidc_client.clone(),
        pending_auth_store.clone(),
        config.clone(),
    );

    let redirect_path = sanitize_redirect_path(req.query::<String>("redirect").as_deref());
    let begin = match oidc_client.begin_auth() {
        Ok(begin) => begin,
        Err(e) => {
            res.status_code(StatusCode::SERVICE_UNAVAILABLE);
            res.render(Text::Plain(format!("OIDC login unavailable: {e}")));
            return;
        }
    };

    if let Err(e) = pending_auth_store
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
    {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Plain(format!("Failed to start OIDC login: {e}")));
        return;
    }

    res.add_cookie(build_cookie(
        STATE_COOKIE,
        begin.state,
        "/v1/auth",
        config.cookie_secure,
        CookieDuration::seconds(STATE_TTL_SECS as i64),
    ));

    let Ok(location) = begin.auth_url.parse() else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Plain(
            "OIDC provider returned an invalid authorization URL",
        ));
        return;
    };
    res.status_code(StatusCode::FOUND);
    res.headers_mut().insert("Location", location);
}

/// Completes the Authorization Code + PKCE exchange, JIT-provisions or
/// looks up the user, mints a session, and redirects back into the web app.
#[endpoint(
    tags("auth"),
    parameters(
        ("state" = Option<String>, Query),
        ("code" = Option<String>, Query),
        ("error" = Option<String>, Query),
        ("error_description" = Option<String>, Query),
    ),
)]
pub async fn oidc_callback(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), OidcCallbackError> {
    let oidc_client = obtain_dep::<Arc<dyn OidcClient>>(depot)?.clone();
    let pending_auth_store = obtain_dep::<Arc<dyn PendingAuthStore>>(depot)?.clone();
    let session_store = obtain_dep::<Arc<dyn SessionStore>>(depot)?.clone();
    let user_repo = obtain_dep::<Arc<dyn UserRepository>>(depot)?.clone();
    let config = obtain_dep::<OidcRuntimeConfig>(depot)?.clone();

    if let Some(error) = req.query::<String>("error") {
        let description = req.query::<String>("error_description").unwrap_or_default();
        return Err(OidcCallbackError::BadRequest(format!(
            "IdP returned error: {error} {description}"
        )));
    }

    let state_cookie = req
        .cookie(STATE_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| OidcCallbackError::BadRequest("Missing state cookie".into()))?;
    res.remove_cookie(STATE_COOKIE);

    let query_state = req
        .query::<String>("state")
        .ok_or_else(|| OidcCallbackError::BadRequest("Missing state parameter".into()))?;

    if state_cookie != query_state {
        return Err(OidcCallbackError::BadRequest(
            "State mismatch between cookie and callback".into(),
        ));
    }

    let pending = pending_auth_store
        .consume(&query_state)
        .await
        .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?
        .ok_or_else(|| {
            OidcCallbackError::BadRequest("Unknown, already-used, or expired login attempt".into())
        })?;

    let code = req
        .query::<String>("code")
        .ok_or_else(|| OidcCallbackError::BadRequest("Missing code parameter".into()))?;

    let identity = oidc_client
        .exchange_code(&code, &pending.pkce_verifier, &pending.nonce)
        .await
        .map_err(|e| match e {
            OidcError::NonceMismatch => OidcCallbackError::BadRequest("Nonce mismatch".to_string()),
            other => OidcCallbackError::BadRequest(format!("Login failed: {other}")),
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
        .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?
    {
        Some(existing) => {
            // A disabled account is blocked at the door: no session is minted
            // and no profile/admin fields are touched (issue #85). Only an
            // already-provisioned account can be disabled -- JIT-provisioned
            // new users below are always created enabled.
            if existing.disabled {
                return Err(OidcCallbackError::Forbidden(
                    "This account has been disabled. Contact an administrator.".to_string(),
                ));
            }
            if existing.is_admin != is_admin {
                user_repo
                    .set_admin(existing.id, is_admin)
                    .await
                    .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?;
            }
            if existing.display_name != display_name || existing.avatar_url != identity.picture {
                user_repo
                    .update_oidc_profile(existing.id, display_name, identity.picture.clone())
                    .await
                    .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?;
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
            .map_err(|e| {
                OidcCallbackError::InternalError(format!("Failed to provision user: {e}"))
            })?,
    };

    let device_hash = device_hash_from_request(req);
    let ip = extract_client_ip(req);
    let idle_ttl_secs = config.session_idle_days * 24 * 60 * 60;
    let absolute_ttl_secs = config.session_max_days * 24 * 60 * 60;

    let session_data = SessionData {
        user_id: user.id.to_string(),
        device_hash,
        ip,
        created_at: Utc::now().timestamp(),
        last_active: Utc::now().timestamp(),
    };

    let token = session_store
        .create(&session_data, idle_ttl_secs, absolute_ttl_secs)
        .await
        .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?;

    res.add_cookie(build_cookie(
        SESSION_COOKIE,
        token,
        "/",
        config.cookie_secure,
        CookieDuration::seconds(absolute_ttl_secs as i64),
    ));

    let redirect_path = pending.redirect_path.unwrap_or_else(|| "/".to_string());
    res.status_code(StatusCode::FOUND);
    res.headers_mut().insert(
        "Location",
        format!("{}{}", config.web_url, redirect_path)
            .parse()
            .map_err(|_| OidcCallbackError::InternalError("Invalid redirect URL".into()))?,
    );

    Ok(())
}

/// Returns the currently authenticated user (via the `beam_session` cookie).
#[endpoint(tags("auth"))]
pub async fn oidc_me(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<MeResponse>, OidcAuthError> {
    let user_repo = obtain_dep::<Arc<dyn UserRepository>>(depot)?.clone();
    let (user_id, _token) = require_web_session(req, depot).await?;

    let user_uuid =
        Uuid::parse_str(&user_id).map_err(|e| OidcAuthError::InternalError(e.to_string()))?;
    let user = user_repo
        .find_by_id(user_uuid)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?
        .ok_or_else(|| OidcAuthError::Unauthorized("User no longer exists".into()))?;

    Ok(Json(MeResponse {
        id: user.id.to_string(),
        email: user.email,
        is_admin: user.is_admin,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }))
}

/// Logs out the current session (deletes it and clears the cookie).
#[endpoint(tags("auth"))]
pub async fn oidc_logout(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if let Some(token) = req.cookie(SESSION_COOKIE).map(|c| c.value().to_string())
        && let Ok(session_store) = obtain_dep::<Arc<dyn SessionStore>>(depot)
    {
        let _ = session_store.delete(&token).await;
    }
    res.remove_cookie(SESSION_COOKIE);
    res.status_code(StatusCode::NO_CONTENT);
}

/// Logs out every active session for the current user.
#[endpoint(tags("auth"))]
pub async fn oidc_logout_all(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), OidcAuthError> {
    let session_store = obtain_dep::<Arc<dyn SessionStore>>(depot)?.clone();
    let (user_id, _token) = require_web_session(req, depot).await?;

    session_store
        .delete_all_for_user(&user_id)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?;

    res.remove_cookie(SESSION_COOKIE);
    res.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

/// Lists every active session for the current user.
#[endpoint(tags("auth"))]
pub async fn oidc_list_sessions(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<SessionSummary>>, OidcAuthError> {
    let session_store = obtain_dep::<Arc<dyn SessionStore>>(depot)?.clone();
    let (user_id, _token) = require_web_session(req, depot).await?;

    let sessions = session_store
        .list_for_user(&user_id)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?;

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
#[endpoint(
    tags("auth"),
    parameters(("id" = String, description = "Session id, from GET /sessions")),
)]
pub async fn oidc_delete_session(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), OidcAuthError> {
    let session_store = obtain_dep::<Arc<dyn SessionStore>>(depot)?.clone();
    let (user_id, current_token) = require_web_session(req, depot).await?;

    let id: String = req.param::<String>("id").unwrap_or_default();
    let deleted = session_store
        .delete_by_id(&id, &user_id)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?;

    if !deleted {
        return Err(OidcAuthError::Unauthorized("Session not found".to_string()));
    }

    // Revoking the session the caller is currently using should also clear
    // their cookie, rather than leaving a dead cookie around.
    if let Some(current) = req.cookie(SESSION_COOKIE)
        && current.value() == current_token
    {
        res.remove_cookie(SESSION_COOKIE);
    }

    res.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

/// Assembles the OIDC routes as a standalone router, at the paths this
/// module's own tests exercise. beam-server does *not* use this -- it
/// mounts the handlers above individually, split between `/v1/auth/*`
/// (login/callback) and top-level `/v1/*` (me/logout/sessions), matching
/// the final ratified endpoint shape now that no legacy routes remain to
/// coexist with.
#[cfg(test)]
pub fn oidc_routes() -> Router {
    Router::new()
        .push(Router::with_path("login").get(oidc_login))
        .push(Router::with_path("callback").get(oidc_callback))
        .push(Router::with_path("me").get(oidc_me))
        .push(Router::with_path("logout").post(oidc_logout))
        .push(Router::with_path("logout-all").post(oidc_logout_all))
        .push(Router::with_path("sessions").get(oidc_list_sessions))
        .push(Router::with_path("sessions/{id}").delete(oidc_delete_session))
}

#[cfg(test)]
#[path = "oidc_routes_tests.rs"]
mod oidc_routes_tests;
