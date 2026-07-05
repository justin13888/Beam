use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::Serialize;

#[derive(Serialize, ToSchema)]
pub struct HealthStatus {
    pub status: String,
    pub timestamp: String,
    pub version: String,
}

/// Health check endpoint
#[endpoint(
    tags("health"),
    responses(
        (status_code = 200, description = "Service is healthy"),
        (status_code = 500, description = "Service is unhealthy")
    )
)]
#[tracing::instrument]
pub async fn health_check(res: &mut Response) {
    res.render(Json(HealthStatus {
        status: "healthy".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }));
}
