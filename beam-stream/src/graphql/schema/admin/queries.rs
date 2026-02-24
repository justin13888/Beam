use async_graphql::*;

use crate::graphql::guard::AdminGuard;
use crate::graphql::AuthGuard;
use crate::models::domain::admin_log::{AdminLog, AdminLogCategory, AdminLogLevel};
use crate::services::notification::AdminEvent;
use crate::state::AppState;

#[derive(SimpleObject)]
pub struct AdminLogEntry {
    pub id: ID,
    pub level: AdminLogLevelGql,
    pub category: String,
    pub message: String,
    pub details: Option<String>,
    pub created_at: String,
}

#[derive(Enum, Copy, Clone, PartialEq, Eq)]
pub enum AdminLogLevelGql {
    Info,
    Warning,
    Error,
}

impl From<AdminLogLevel> for AdminLogLevelGql {
    fn from(level: AdminLogLevel) -> Self {
        match level {
            AdminLogLevel::Info => AdminLogLevelGql::Info,
            AdminLogLevel::Warning => AdminLogLevelGql::Warning,
            AdminLogLevel::Error => AdminLogLevelGql::Error,
        }
    }
}

fn category_to_str(cat: &AdminLogCategory) -> &'static str {
    match cat {
        AdminLogCategory::LibraryScan => "library_scan",
        AdminLogCategory::System => "system",
        AdminLogCategory::Auth => "auth",
    }
}

impl From<AdminLog> for AdminLogEntry {
    fn from(log: AdminLog) -> Self {
        Self {
            id: log.id.to_string().into(),
            level: log.level.into(),
            category: category_to_str(&log.category).to_string(),
            message: log.message,
            details: log.details.map(|v| v.to_string()),
            created_at: log.created_at.to_rfc3339(),
        }
    }
}

#[derive(Default)]
pub struct AdminQuery;

#[Object]
impl AdminQuery {
    /// Fetch recent admin events from the in-memory event log.
    /// Returns the most recent `limit` events (default 100, max 1000).
    #[graphql(guard = "AuthGuard")]
    async fn admin_events(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u32,
    ) -> Result<Vec<AdminEvent>> {
        let state = ctx.data::<AppState>()?;
        let limit = (limit as usize).min(1000);
        let events = state.services.notification.recent_events(limit);
        Ok(events)
    }

    /// Fetch paginated admin log entries (most recent first). Admin only.
    #[graphql(guard = "AdminGuard")]
    async fn logs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u32,
        #[graphql(default = 0)] offset: u32,
    ) -> Result<Vec<AdminLogEntry>> {
        let state = ctx.data::<AppState>()?;
        let logs = state
            .services
            .admin_log
            .get_logs(limit, offset)
            .await
            .map_err(|e| Error::new(e.to_string()))?;
        Ok(logs.into_iter().map(AdminLogEntry::from).collect())
    }

    /// Total count of admin log entries. Admin only.
    #[graphql(guard = "AdminGuard")]
    async fn log_count(&self, ctx: &Context<'_>) -> Result<u64> {
        let state = ctx.data::<AppState>()?;
        let count = state
            .services
            .admin_log
            .count()
            .await
            .map_err(|e| Error::new(e.to_string()))?;
        Ok(count)
    }
}
