#[cfg(test)]
#[path = "routes_tests.rs"]
mod routes_tests;

use crate::utils::service::AuthService;
use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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

#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username_or_email: String,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RefreshRequest {
    pub session_id: String,
}

/// Register a new user account
#[endpoint(
    tags("auth"),
    request_body = RegisterRequest,
    responses(
        (status_code = 200, body = crate::utils::service::AuthResponse, description = "User registered successfully"),
        (status_code = 400, description = "Invalid request or user already exists")
    )
)]
pub async fn register(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();
    let body: RegisterRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(_) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain("Invalid request body"));
            return;
        }
    };

    let device_hash = device_hash_from_request(req);
    let ip = extract_client_ip(req);

    match auth
        .register(
            &body.username,
            &body.email,
            &body.password,
            &device_hash,
            &ip,
        )
        .await
    {
        Ok(auth_response) => {
            let cookie = salvo::http::cookie::Cookie::build((
                "session_id",
                auth_response.session_id.clone(),
            ))
            .path("/")
            .http_only(true)
            .same_site(salvo::http::cookie::SameSite::Lax)
            .max_age(salvo::http::cookie::time::Duration::days(7))
            .build();
            res.add_cookie(cookie);
            res.status_code(StatusCode::OK);
            res.render(Json(auth_response));
        }
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

/// Login with username/email and password
#[endpoint(
    tags("auth"),
    request_body = LoginRequest,
    responses(
        (status_code = 200, body = crate::utils::service::AuthResponse, description = "Login successful"),
        (status_code = 400, description = "Bad request"),
        (status_code = 401, description = "Invalid credentials")
    )
)]
pub async fn login(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();
    let body: LoginRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(_) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain("Invalid request body"));
            return;
        }
    };

    let device_hash = device_hash_from_request(req);
    let ip = extract_client_ip(req);

    match auth
        .login(&body.username_or_email, &body.password, &device_hash, &ip)
        .await
    {
        Ok(auth_response) => {
            let cookie = salvo::http::cookie::Cookie::build((
                "session_id",
                auth_response.session_id.clone(),
            ))
            .path("/")
            .http_only(true)
            .same_site(salvo::http::cookie::SameSite::Lax)
            .max_age(salvo::http::cookie::time::Duration::days(7))
            .build();
            res.add_cookie(cookie);
            res.status_code(StatusCode::OK);
            res.render(Json(auth_response));
        }
        Err(err) => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

/// Refresh an existing session using a session cookie or request body
#[endpoint(
    tags("auth"),
    request_body(content = RefreshRequest, description = "Session ID (alternative to session cookie)"),
    responses(
        (status_code = 200, body = crate::utils::service::AuthResponse, description = "Session refreshed successfully"),
        (status_code = 401, description = "Invalid or expired session")
    )
)]
pub async fn refresh(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();

    let session_id = if let Some(c) = req.cookie("session_id") {
        c.value().to_string()
    } else if let Ok(body) = req.parse_json::<RefreshRequest>().await {
        body.session_id
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Plain("Missing session cookie or body"));
        return;
    };

    match auth.refresh(&session_id).await {
        Ok(auth_response) => {
            let cookie = salvo::http::cookie::Cookie::build((
                "session_id",
                auth_response.session_id.clone(),
            ))
            .path("/")
            .http_only(true)
            .same_site(salvo::http::cookie::SameSite::Lax)
            .max_age(salvo::http::cookie::time::Duration::days(7))
            .build();
            res.add_cookie(cookie);

            res.status_code(StatusCode::OK);
            res.render(Json(auth_response));
        }
        Err(err) => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

/// Logout and revoke the current session
#[endpoint(
    tags("auth"),
    responses(
        (status_code = 200, description = "Logged out successfully"),
        (status_code = 500, description = "Internal server error")
    )
)]
pub async fn logout(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();

    let session_id = if let Some(c) = req.cookie("session_id") {
        c.value().to_string()
    } else if let Ok(body) = req.parse_json::<RefreshRequest>().await {
        body.session_id
    } else {
        // Already logged out or no session
        res.status_code(StatusCode::OK);
        return;
    };

    // Remove cookie
    res.remove_cookie("session_id");

    match auth.logout(&session_id).await {
        Ok(_) => {
            res.status_code(StatusCode::OK);
        }
        Err(err) => {
            // Even if backend fails, we cleared the cookie
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct LogoutAllResponse {
    /// Number of sessions that were revoked
    pub revoked: u64,
}

#[derive(Serialize, ToSchema)]
pub struct SessionSummary {
    pub session_id: String,
    pub device_hash: String,
    pub ip: String,
    pub created_at: i64,
    pub last_active: i64,
}

fn extract_bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.starts_with("Bearer "))
        .map(|s| s[7..].to_string())
}

/// Logout all active sessions for the current user
#[endpoint(
    tags("auth"),
    responses(
        (status_code = 200, body = LogoutAllResponse, description = "All sessions revoked"),
        (status_code = 401, description = "Invalid or missing JWT")
    )
)]
pub async fn logout_all(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();

    let token = match extract_bearer_token(req) {
        Some(t) => t,
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            return;
        }
    };

    let user = match auth.verify_token(&token).await {
        Ok(u) => u,
        Err(_) => {
            res.status_code(StatusCode::UNAUTHORIZED);
            return;
        }
    };

    match auth.logout_all(&user.user_id).await {
        Ok(revoked) => {
            res.status_code(StatusCode::OK);
            res.render(Json(LogoutAllResponse { revoked }));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

/// List all active sessions for the current user
#[endpoint(
    tags("auth"),
    responses(
        (status_code = 200, body = Vec<SessionSummary>, description = "Active sessions"),
        (status_code = 401, description = "Invalid or missing JWT")
    )
)]
pub async fn list_sessions(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = depot.obtain::<Arc<dyn AuthService>>().unwrap().clone();

    let token = match extract_bearer_token(req) {
        Some(t) => t,
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            return;
        }
    };

    let user = match auth.verify_token(&token).await {
        Ok(u) => u,
        Err(_) => {
            res.status_code(StatusCode::UNAUTHORIZED);
            return;
        }
    };

    match auth.get_sessions(&user.user_id).await {
        Ok(sessions) => {
            let summaries: Vec<SessionSummary> = sessions
                .into_iter()
                .map(|(session_id, data)| SessionSummary {
                    session_id,
                    device_hash: data.device_hash,
                    ip: data.ip,
                    created_at: data.created_at,
                    last_active: data.last_active,
                })
                .collect();
            res.status_code(StatusCode::OK);
            res.render(Json(summaries));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(err.to_string()));
        }
    }
}

pub fn auth_routes() -> Router {
    Router::new()
        .push(Router::with_path("register").post(register))
        .push(Router::with_path("login").post(login))
        .push(Router::with_path("refresh").post(refresh))
        .push(Router::with_path("logout").post(logout))
        .push(Router::with_path("logout-all").post(logout_all))
        .push(Router::with_path("sessions").get(list_sessions))
}
