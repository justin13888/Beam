use crate::utils::service::AuthService;
use salvo::prelude::*;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username_or_email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub session_id: String,
}

#[handler]
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

    // TODO: Get device info from headers
    let device_hash = "unknown_device";
    let ip = "0.0.0.0";

    match auth
        .register(&body.username, &body.email, &body.password, device_hash, ip)
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

#[handler]
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

    // TODO: Get device info from headers
    let device_hash = "unknown_device";
    let ip = "0.0.0.0";

    match auth
        .login(&body.username_or_email, &body.password, device_hash, ip)
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

#[handler]
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

#[handler]
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

pub fn auth_routes() -> Router {
    Router::new()
        .push(Router::with_path("register").post(register))
        .push(Router::with_path("login").post(login))
        .push(Router::with_path("refresh").post(refresh))
        .push(Router::with_path("logout").post(logout))
}
