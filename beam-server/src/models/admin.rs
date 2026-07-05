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
