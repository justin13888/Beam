use uuid::Uuid;

use crate::state::{AppContext, AppState};
use async_graphql::{Context, Guard, Result};

pub struct AuthGuard;

impl Guard for AuthGuard {
    async fn check(&self, ctx: &Context<'_>) -> Result<()> {
        let app_ctx = ctx.data::<AppContext>().map_err(|_| "AppContext missing")?;
        if app_ctx.user_context().is_some() {
            Ok(())
        } else {
            Err("Unauthorized".into())
        }
    }
}

pub struct AdminGuard;

impl Guard for AdminGuard {
    async fn check(&self, ctx: &Context<'_>) -> Result<()> {
        let app_ctx = ctx.data::<AppContext>().map_err(|_| "AppContext missing")?;
        let user_ctx = app_ctx
            .user_context()
            .ok_or_else(|| async_graphql::Error::new("Unauthorized"))?;

        let state = ctx.data::<AppState>().map_err(|_| "AppState missing")?;
        let user_id = Uuid::parse_str(&user_ctx.user_id)
            .map_err(|_| async_graphql::Error::new("Invalid user ID"))?;

        let user = state
            .services
            .user_repo
            .find_by_id(user_id)
            .await
            .map_err(|e| async_graphql::Error::new(format!("Database error: {e}")))?
            .ok_or_else(|| async_graphql::Error::new("User not found"))?;

        if user.is_admin {
            Ok(())
        } else {
            Err("Forbidden: admin access required".into())
        }
    }
}
