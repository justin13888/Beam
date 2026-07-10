//! Shared v1 REST conventions: a single JSON error body shape and a
//! cookie-session `CurrentUser` extractor, reused across every `/v1`
//! endpoint so new routes don't reinvent auth parsing or error formatting.
//!
//! Auth is the `beam_session` cookie set by the OIDC login flow (see
//! ADR-0003) -- looked up via `SessionStore::get`, sliding the idle expiry
//! forward on activity the same way `beam_auth`'s own `/me` endpoint does.

use async_trait::async_trait;
use beam_auth::utils::session_store::get_and_touch;
use salvo::http::StatusCode;
use salvo::oapi::{ToResponses, ToSchema};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const SESSION_COOKIE: &str = "beam_session";

/// Uniform JSON error body: `{"error": "message"}` for every `/v1` endpoint.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiErrorBody {
    pub error: String,
}

#[derive(Debug, ToResponses)]
pub enum ApiError {
    /// Bad request
    #[salvo(response(status_code = 400))]
    BadRequest(String),
    /// Unauthorized
    #[salvo(response(status_code = 401))]
    Unauthorized(String),
    /// Forbidden
    #[salvo(response(status_code = 403))]
    Forbidden(String),
    /// Not found
    #[salvo(response(status_code = 404))]
    NotFound(String),
    /// Internal server error
    #[salvo(response(status_code = 500))]
    Internal(String),
}

#[async_trait]
impl Writer for ApiError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        let (status, message) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        res.status_code(status);
        res.render(Json(ApiErrorBody { error: message }));
    }
}

/// The caller's identity, resolved from the `beam_session` cookie.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: String,
}

/// Fetches the injected [`AppState`] from the depot. The state is wired in
/// by `create_router`'s `affix_state` hoop, so a miss is a router wiring
/// bug -- surfaced as a 500 rather than a handler panic.
pub fn obtain_state(depot: &Depot) -> Result<&AppState, ApiError> {
    depot.obtain::<AppState>().map_err(|_| {
        tracing::error!("AppState missing from depot -- router wiring bug");
        ApiError::Internal("Server state unavailable".to_string())
    })
}

/// Extract the caller's identity from the `beam_session` cookie.
pub async fn require_auth(req: &Request, state: &AppState) -> Result<AuthenticatedUser, ApiError> {
    let token = req
        .cookie(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| ApiError::Unauthorized("Missing session cookie".to_string()))?;

    let idle_ttl_secs = state.config.session_idle_days * 24 * 60 * 60;
    let session = get_and_touch(state.services.session_store.as_ref(), &token, idle_ttl_secs)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::Unauthorized("Invalid or expired session".to_string()))?;

    Ok(AuthenticatedUser {
        user_id: session.user_id,
    })
}

/// Like [`require_auth`], but additionally requires the caller to be an
/// admin. Every admin-gated `/v1/admin/*` route uses this.
pub async fn require_admin(req: &Request, state: &AppState) -> Result<AuthenticatedUser, ApiError> {
    let user = require_auth(req, state).await?;

    let user_id = uuid::Uuid::parse_str(&user.user_id)
        .map_err(|_| ApiError::Internal("invalid user id in session".to_string()))?;
    let db_user = state
        .services
        .user_repo
        .find_by_id(user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::Unauthorized("user no longer exists".to_string()))?;

    if db_user.is_admin {
        Ok(user)
    } else {
        Err(ApiError::Forbidden("admin access required".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use salvo::test::ResponseExt;

    #[tokio::test]
    async fn api_error_renders_json_body_with_matching_status() {
        let mut res = Response::new();
        ApiError::NotFound("nope".to_string())
            .write(&mut Request::default(), &mut Depot::new(), &mut res)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
        let body: ApiErrorBody = res.take_json().await.unwrap();
        assert_eq!(body.error, "nope");
    }

    #[tokio::test]
    async fn api_error_unauthorized_renders_401() {
        let mut res = Response::new();
        ApiError::Unauthorized("missing token".to_string())
            .write(&mut Request::default(), &mut Depot::new(), &mut res)
            .await;
        assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    }
}
