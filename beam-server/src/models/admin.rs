//! REST-facing DTOs for the admin API. These wrap beam-index's
//! `AdminEvent`/`EventLevel`/`EventCategory` and beam-domain's
//! `AdminLog`/`AdminLogLevel`/`AdminLogCategory` rather than adding
//! `serde`/`salvo::oapi` derives directly to those crates, which would pull a
//! web-framework dependency into the indexer/domain layers.

use beam_domain::models::admin_log::{AdminLog, AdminLogCategory, AdminLogLevel};
use beam_index::services::notification::{AdminEvent, EventCategory, EventLevel};
use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminEventLevelDto {
    Info,
    Warning,
    Error,
}

impl From<EventLevel> for AdminEventLevelDto {
    fn from(level: EventLevel) -> Self {
        match level {
            EventLevel::Info => AdminEventLevelDto::Info,
            EventLevel::Warning => AdminEventLevelDto::Warning,
            EventLevel::Error => AdminEventLevelDto::Error,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminEventCategoryDto {
    LibraryScan,
    System,
}

impl From<EventCategory> for AdminEventCategoryDto {
    fn from(category: EventCategory) -> Self {
        match category {
            EventCategory::LibraryScan => AdminEventCategoryDto::LibraryScan,
            EventCategory::System => AdminEventCategoryDto::System,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AdminEventDto {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub level: AdminEventLevelDto,
    pub category: AdminEventCategoryDto,
    pub message: String,
    pub library_id: Option<String>,
    pub library_name: Option<String>,
}

impl From<AdminEvent> for AdminEventDto {
    fn from(event: AdminEvent) -> Self {
        Self {
            id: event.id,
            timestamp: event.timestamp,
            level: event.level.into(),
            category: event.category.into(),
            message: event.message,
            library_id: event.library_id,
            library_name: event.library_name,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminLogLevelDto {
    Info,
    Warning,
    Error,
}

impl From<AdminLogLevel> for AdminLogLevelDto {
    fn from(level: AdminLogLevel) -> Self {
        match level {
            AdminLogLevel::Info => AdminLogLevelDto::Info,
            AdminLogLevel::Warning => AdminLogLevelDto::Warning,
            AdminLogLevel::Error => AdminLogLevelDto::Error,
        }
    }
}

fn category_to_str(category: &AdminLogCategory) -> &'static str {
    match category {
        AdminLogCategory::LibraryScan => "library_scan",
        AdminLogCategory::System => "system",
        AdminLogCategory::Auth => "auth",
        AdminLogCategory::Enrichment => "enrichment",
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminLogEntryDto {
    pub id: String,
    pub level: AdminLogLevelDto,
    pub category: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
    pub created_at: String,
}

impl From<AdminLog> for AdminLogEntryDto {
    fn from(log: AdminLog) -> Self {
        Self {
            id: log.id.to_string(),
            level: log.level.into(),
            category: category_to_str(&log.category).to_string(),
            message: log.message,
            details: log.details,
            created_at: log.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateLibraryRequest {
    pub name: String,
    pub root_path: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScanLibraryResponse {
    pub added: u32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminLogCountResponse {
    pub count: u64,
}

// ── Admin user management (issue #85) ────────────────────────────────────────

/// One user row in the admin users tab. `is_admin` is informational and
/// read-only here: admin is derived from the IdP-asserted claim on every
/// login, so there is deliberately no endpoint to change it.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminUserDto {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    /// Local moderation switch (see `PATCH /v1/admin/users/{id}`): a disabled
    /// user cannot log in and their sessions are revoked on disable.
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

impl From<beam_auth::utils::models::User> for AdminUserDto {
    fn from(user: beam_auth::utils::models::User) -> Self {
        let beam_auth::utils::models::User {
            id,
            oidc_issuer: _,
            oidc_subject: _,
            email,
            display_name,
            avatar_url,
            is_admin,
            disabled,
            created_at,
            updated_at: _,
        } = user;
        Self {
            id: id.to_string(),
            display_name,
            email,
            avatar_url,
            is_admin,
            disabled,
            created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminUserListResponse {
    pub items: Vec<AdminUserDto>,
    /// Total number of users across all pages.
    pub total: u64,
}

/// Body of `PATCH /v1/admin/users/{id}`. `disabled` is the only mutable
/// field: `is_admin` is IdP-claim-driven (recomputed at every login), so a
/// local toggle would be silently overwritten and is deliberately absent.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateAdminUserRequest {
    pub disabled: bool,
}

// ── Admin system status (issue #85) ──────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminStatusCounts {
    pub users: u64,
    pub libraries: u64,
    pub files: u64,
}

/// Metadata-enrichment queue overview: row counts per state.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnrichmentQueueCounts {
    pub pending: u64,
    pub enriched: u64,
    pub unmatched: u64,
    pub failed: u64,
}

impl From<beam_domain::models::enrichment::EnrichmentStatusCounts> for EnrichmentQueueCounts {
    fn from(counts: beam_domain::models::enrichment::EnrichmentStatusCounts) -> Self {
        let beam_domain::models::enrichment::EnrichmentStatusCounts {
            pending,
            enriched,
            unmatched,
            failed,
        } = counts;
        Self {
            pending,
            enriched,
            unmatched,
            failed,
        }
    }
}

/// One recent library-scan admin log entry, slimmed to what the system
/// status tab renders.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RecentScanDto {
    pub level: AdminLogLevelDto,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

impl From<AdminLog> for RecentScanDto {
    fn from(log: AdminLog) -> Self {
        let AdminLog {
            id: _,
            level,
            category: _,
            message,
            details: _,
            created_at,
        } = log;
        Self {
            level: level.into(),
            message,
            timestamp: created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminStatusResponse {
    /// Whole seconds since the server process built its state.
    pub uptime_secs: u64,
    /// Server crate version (`CARGO_PKG_VERSION`).
    pub version: String,
    pub counts: AdminStatusCounts,
    pub enrichment: EnrichmentQueueCounts,
    /// Most recent `library_scan` admin log entries, newest first.
    pub recent_scans: Vec<RecentScanDto>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn admin_event_dto_maps_all_fields() {
        let event = AdminEvent {
            id: "evt-1".to_string(),
            timestamp: Utc::now(),
            level: EventLevel::Warning,
            category: EventCategory::LibraryScan,
            message: "scan finished".to_string(),
            library_id: Some("lib-1".to_string()),
            library_name: Some("Movies".to_string()),
        };
        let dto = AdminEventDto::from(event.clone());
        assert_eq!(dto.id, "evt-1");
        assert!(matches!(dto.level, AdminEventLevelDto::Warning));
        assert!(matches!(dto.category, AdminEventCategoryDto::LibraryScan));
        assert_eq!(dto.message, "scan finished");
        assert_eq!(dto.library_id, Some("lib-1".to_string()));
    }

    #[test]
    fn admin_log_entry_dto_maps_category_to_snake_case_string() {
        let log = AdminLog {
            id: Uuid::new_v4(),
            level: AdminLogLevel::Error,
            category: AdminLogCategory::Enrichment,
            message: "match failed".to_string(),
            details: None,
            created_at: Utc::now(),
        };
        let dto = AdminLogEntryDto::from(log);
        assert_eq!(dto.category, "enrichment");
        assert!(matches!(dto.level, AdminLogLevelDto::Error));
        assert_eq!(dto.message, "match failed");
    }
}
