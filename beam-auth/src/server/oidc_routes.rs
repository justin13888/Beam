//! OIDC BFF endpoints (see ADR-0003): `login`/`callback` drive the
//! Authorization Code + PKCE round-trip, `me`/`logout`/`logout-all`/
//! `sessions`/`sessions/{id}` operate on the resulting `beam_session`
//! cookie. Mounted at `/v1/auth/oidc/*`, alongside (not replacing) the
//! legacy password endpoints at `/v1/auth/*` -- coexistence is temporary,
//! until the auth cutover deletes the password flow and these move up to
//! take over the primary `/v1/auth/*` paths.
//!
//! The browser never sees an IdP token; `beam_session` is the only
//! credential it holds, set as an httpOnly, `SameSite=Lax` cookie.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use async_trait::async_trait;
use chrono::Utc;
use salvo::http::cookie::{Cookie, SameSite, time::Duration as CookieDuration};
use salvo::oapi::{ToResponses, ToSchema};
use salvo::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::server::routes::{SessionSummary, device_hash_from_request, extract_client_ip};
use crate::utils::admin_allowlist::is_admin_email;
use crate::utils::models::CreateUser;
use crate::utils::oidc::{OidcClient, OidcError};
use crate::utils::pending_auth_store::{PendingAuth, PendingAuthStore};
use crate::utils::repository::UserRepository;
use crate::utils::session_store::{SessionData, SessionStore};

const STATE_COOKIE: &str = "beam_oidc_state";
const SESSION_COOKIE: &str = "beam_session";
const STATE_TTL_SECS: u64 = 600; // 10 minutes to complete the round trip
const TOUCH_THROTTLE_SECS: i64 = 3600; // touch the idle expiry at most once/hour

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
    /// Comma-separated admin email allowlist (see `admin_allowlist`).
    pub admin_emails_csv: String,
    pub session_idle_days: u64,
    pub session_max_days: u64,
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

/// Generates a password hash for a random value nobody can ever type --
/// OIDC-provisioned users authenticate via the IdP only, but the legacy
/// `users.password_hash` column stays `NOT NULL` while password auth and
/// OIDC auth coexist (see ADR-0003; the column is dropped at the auth
/// cutover).
fn unusable_password_hash() -> String {
    use argon2::{
        Argon2,
        password_hash::{PasswordHasher, SaltString},
    };

    let mut random_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut random_bytes);
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(&random_bytes, &salt)
        .expect("hashing random bytes never fails")
        .to_string()
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub id: String,
    pub username: String,
    pub email: String,
    pub is_admin: bool,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(ToResponses)]
pub enum OidcCallbackError {
    /// The state/nonce/PKCE round-trip failed verification.
    #[salvo(response(status_code = 400))]
    BadRequest(String),
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

/// Resolves the current user from the `beam_session` cookie, sliding the
/// idle expiry forward (throttled to at most once/hour so every request
/// isn't a write).
async fn require_web_session(
    req: &Request,
    depot: &Depot,
) -> Result<(String, String), OidcAuthError> {
    let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap();
    let config = depot.obtain::<OidcRuntimeConfig>().unwrap();

    let token = req
        .cookie(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| OidcAuthError::Unauthorized("Missing session cookie".into()))?;

    let session = session_store
        .get(&token)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?
        .ok_or_else(|| OidcAuthError::Unauthorized("Invalid or expired session".into()))?;

    if Utc::now().timestamp() - session.last_active > TOUCH_THROTTLE_SECS {
        let idle_ttl_secs = config.session_idle_days * 24 * 60 * 60;
        let _ = session_store.touch(&token, idle_ttl_secs).await;
    }

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
    let oidc_client = depot.obtain::<Arc<dyn OidcClient>>().unwrap().clone();
    let pending_auth_store = depot.obtain::<Arc<dyn PendingAuthStore>>().unwrap().clone();
    let config = depot.obtain::<OidcRuntimeConfig>().unwrap().clone();

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
        "/v1/auth/oidc",
        config.cookie_secure,
        CookieDuration::seconds(STATE_TTL_SECS as i64),
    ));

    res.status_code(StatusCode::FOUND);
    res.headers_mut()
        .insert("Location", begin.auth_url.parse().unwrap());
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
    let oidc_client = depot.obtain::<Arc<dyn OidcClient>>().unwrap().clone();
    let pending_auth_store = depot.obtain::<Arc<dyn PendingAuthStore>>().unwrap().clone();
    let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap().clone();
    let user_repo = depot.obtain::<Arc<dyn UserRepository>>().unwrap().clone();
    let config = depot.obtain::<OidcRuntimeConfig>().unwrap().clone();

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

    let email = identity
        .email
        .clone()
        .ok_or_else(|| OidcCallbackError::BadRequest("IdP did not provide an email".into()))?;

    let is_admin = identity.email_verified && is_admin_email(&email, &config.admin_emails_csv);

    let user = match user_repo
        .find_by_oidc_identity(&identity.issuer, &identity.subject)
        .await
        .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?
    {
        Some(existing) => {
            if existing.is_admin != is_admin {
                user_repo
                    .set_admin(existing.id, is_admin)
                    .await
                    .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?;
            }
            if existing.display_name != identity.name || existing.avatar_url != identity.picture {
                user_repo
                    .update_oidc_profile(
                        existing.id,
                        identity.name.clone(),
                        identity.picture.clone(),
                    )
                    .await
                    .map_err(|e| OidcCallbackError::InternalError(e.to_string()))?;
            }
            existing
        }
        None => {
            let username = email
                .split('@')
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("user-{}", &identity.subject));

            user_repo
                .create(CreateUser {
                    username,
                    email: email.clone(),
                    password_hash: unusable_password_hash(),
                    is_admin,
                    oidc_issuer: Some(identity.issuer.clone()),
                    oidc_subject: Some(identity.subject.clone()),
                    display_name: identity.name.clone(),
                    avatar_url: identity.picture.clone(),
                })
                .await
                .map_err(|e| {
                    OidcCallbackError::InternalError(format!(
                        "Failed to provision user (username/email conflict?): {e}"
                    ))
                })?
        }
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
    let user_repo = depot.obtain::<Arc<dyn UserRepository>>().unwrap().clone();
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
        username: user.username,
        email: user.email,
        is_admin: user.is_admin,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }))
}

/// Logs out the current session (deletes it and clears the cookie).
#[endpoint(tags("auth"))]
pub async fn oidc_logout(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if let Some(token) = req.cookie(SESSION_COOKIE).map(|c| c.value().to_string()) {
        let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap();
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
    let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap().clone();
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
    let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap().clone();
    let (user_id, _token) = require_web_session(req, depot).await?;

    let sessions = session_store
        .list_for_user(&user_id)
        .await
        .map_err(|e| OidcAuthError::InternalError(e.to_string()))?;

    Ok(Json(
        sessions
            .into_iter()
            .map(|(session_id, data)| SessionSummary {
                session_id,
                device_hash: data.device_hash,
                ip: data.ip,
                created_at: data.created_at,
                last_active: data.last_active,
            })
            .collect(),
    ))
}

/// Revokes a specific session by its listing id, scoped to the current user
/// (returns 404 for a session that doesn't exist or belongs to someone
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
    let session_store = depot.obtain::<Arc<dyn SessionStore>>().unwrap().clone();
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
