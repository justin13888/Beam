use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminLogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminLogCategory {
    LibraryScan,
    System,
    Auth,
}

#[derive(Debug, Clone)]
pub struct AdminLog {
    pub id: Uuid,
    pub level: AdminLogLevel,
    pub category: AdminLogCategory,
    pub message: String,
    pub details: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateAdminLog {
    pub level: AdminLogLevel,
    pub category: AdminLogCategory,
    pub message: String,
    pub details: Option<Value>,
}

impl From<beam_entity::admin_log::AdminLogLevel> for AdminLogLevel {
    fn from(level: beam_entity::admin_log::AdminLogLevel) -> Self {
        match level {
            beam_entity::admin_log::AdminLogLevel::Info => AdminLogLevel::Info,
            beam_entity::admin_log::AdminLogLevel::Warning => AdminLogLevel::Warning,
            beam_entity::admin_log::AdminLogLevel::Error => AdminLogLevel::Error,
        }
    }
}

impl From<AdminLogLevel> for beam_entity::admin_log::AdminLogLevel {
    fn from(level: AdminLogLevel) -> Self {
        match level {
            AdminLogLevel::Info => beam_entity::admin_log::AdminLogLevel::Info,
            AdminLogLevel::Warning => beam_entity::admin_log::AdminLogLevel::Warning,
            AdminLogLevel::Error => beam_entity::admin_log::AdminLogLevel::Error,
        }
    }
}

impl From<beam_entity::admin_log::AdminLogCategory> for AdminLogCategory {
    fn from(cat: beam_entity::admin_log::AdminLogCategory) -> Self {
        match cat {
            beam_entity::admin_log::AdminLogCategory::LibraryScan => AdminLogCategory::LibraryScan,
            beam_entity::admin_log::AdminLogCategory::System => AdminLogCategory::System,
            beam_entity::admin_log::AdminLogCategory::Auth => AdminLogCategory::Auth,
        }
    }
}

impl From<AdminLogCategory> for beam_entity::admin_log::AdminLogCategory {
    fn from(cat: AdminLogCategory) -> Self {
        match cat {
            AdminLogCategory::LibraryScan => beam_entity::admin_log::AdminLogCategory::LibraryScan,
            AdminLogCategory::System => beam_entity::admin_log::AdminLogCategory::System,
            AdminLogCategory::Auth => beam_entity::admin_log::AdminLogCategory::Auth,
        }
    }
}

impl From<beam_entity::admin_log::Model> for AdminLog {
    fn from(model: beam_entity::admin_log::Model) -> Self {
        Self {
            id: model.id,
            level: model.level.into(),
            category: model.category.into(),
            message: model.message,
            details: model.details,
            created_at: model.created_at.into(),
        }
    }
}
