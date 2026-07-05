//! Shared v1 REST conventions: a single JSON error body shape and a
//! Bearer-JWT `CurrentUser` extractor, reused across every `/v1` endpoint so
//! new routes don't reinvent auth parsing or error formatting.
//!
//! Auth here stays Bearer-JWT-compatible deliberately -- the OIDC BFF /
//! cookie-session cutover lands later and will replace `require_auth`'s
//! internals without changing callers' error handling.

use async_trait::async_trait;
use beam_auth::utils::service::AuthenticatedUser;
use salvo::http::StatusCode;
use salvo::oapi::{ToResponses, ToSchema};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

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
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        res.status_code(status);
        res.render(Json(ApiErrorBody { error: message }));
    }
}

/// Extract the caller's identity from a `Bearer <jwt>` Authorization header.
pub async fn require_auth(req: &Request, state: &AppState) -> Result<AuthenticatedUser, ApiError> {
    let token = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::Unauthorized("Missing Authorization header".to_string()))?;

    state
        .services
        .auth
        .verify_token(token)
        .await
        .map_err(|_| ApiError::Unauthorized("Invalid or expired token".to_string()))
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
