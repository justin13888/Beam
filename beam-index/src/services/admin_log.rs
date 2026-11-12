use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use beam_domain::models::{AdminLog, AdminLogCategory, AdminLogLevel, CreateAdminLog};
use beam_domain::repositories::AdminLogRepository;

#[derive(Debug, Error)]
pub enum AdminLogError {
    #[error("Database error: {0}")]
    Database(String),
}

impl From<sea_orm::DbErr> for AdminLogError {
    fn from(e: sea_orm::DbErr) -> Self {
        AdminLogError::Database(e.to_string())
    }
}

type Result<T> = std::result::Result<T, AdminLogError>;

#[async_trait]
pub trait AdminLogService: Send + Sync + std::fmt::Debug {
    async fn log(
        &self,
        level: AdminLogLevel,
        category: AdminLogCategory,
        message: String,
        details: Option<Value>,
    ) -> Result<()>;

    async fn get_logs(&self, limit: u32, offset: u32) -> Result<Vec<AdminLog>>;

    /// Like [`get_logs`](Self::get_logs) (newest first) but restricted to a
    /// single category -- e.g. `LibraryScan` for the admin status endpoint's
    /// recent scan history (issue #85).
    async fn get_logs_by_category(
        &self,
        category: AdminLogCategory,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<AdminLog>>;

    async fn count(&self) -> Result<u64>;
}

#[derive(Debug)]
pub struct LocalAdminLogService {
    repo: Arc<dyn AdminLogRepository>,
}

impl LocalAdminLogService {
    pub fn new(repo: Arc<dyn AdminLogRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl AdminLogService for LocalAdminLogService {
    async fn log(
        &self,
        level: AdminLogLevel,
        category: AdminLogCategory,
        message: String,
        details: Option<Value>,
    ) -> Result<()> {
        self.repo
            .create(CreateAdminLog {
                level,
                category,
                message,
                details,
            })
            .await?;
        Ok(())
    }

    async fn get_logs(&self, limit: u32, offset: u32) -> Result<Vec<AdminLog>> {
        Ok(self.repo.list(limit as u64, offset as u64).await?)
    }

    async fn get_logs_by_category(
        &self,
        category: AdminLogCategory,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<AdminLog>> {
        Ok(self
            .repo
            .list_by_category(category, limit as u64, offset as u64)
            .await?)
    }

    async fn count(&self) -> Result<u64> {
        Ok(self.repo.count().await?)
    }
}

/// Record an event the operator reads in two places: the process log, as a
/// structured `tracing` event, and the admin log the admin viewer pages through
/// (NFR-403).
///
/// Never fails. The event is already in the process log when the admin log is
/// written, so a store that cannot take the row costs the viewer one entry and
/// costs the action that caused it nothing: an administrator's change has been
/// made, and a sign-in has been decided, whether or not the row lands. The
/// failure is itself logged.
///
/// `details` is written as given; it is the caller's to keep it free of
/// session tokens, cookies, IdP tokens and device codes.
pub async fn record(
    admin_log: &dyn AdminLogService,
    level: AdminLogLevel,
    category: AdminLogCategory,
    message: String,
    details: Value,
) {
    match level {
        AdminLogLevel::Info => {
            tracing::info!(category = ?category, details = %details, "{message}");
        }
        AdminLogLevel::Warning => {
            tracing::warn!(category = ?category, details = %details, "{message}");
        }
        AdminLogLevel::Error => {
            tracing::error!(category = ?category, details = %details, "{message}");
        }
    }
    if let Err(err) = admin_log.log(level, category, message, Some(details)).await {
        tracing::warn!(error = %err, "could not write an admin log entry");
    }
}

/// Record what an administrator did, and who: [`record`] at `Info`, with
/// `admin_user_id` added to `details`.
///
/// `details` should be a JSON object; anything else is wrapped as `{"detail":
/// ...}` so the administrator's id is never dropped.
pub async fn record_admin_action(
    admin_log: &dyn AdminLogService,
    category: AdminLogCategory,
    message: String,
    admin_user_id: &str,
    details: Value,
) {
    let mut details = match details {
        Value::Object(fields) => fields,
        other => {
            let mut fields = serde_json::Map::new();
            fields.insert("detail".to_owned(), other);
            fields
        }
    };
    details.insert("admin_user_id".to_owned(), Value::from(admin_user_id));
    record(
        admin_log,
        AdminLogLevel::Info,
        category,
        message,
        Value::Object(details),
    )
    .await;
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// No-op admin log service for tests that don't care about logging
    #[derive(Debug)]
    pub struct NoOpAdminLogService;

    #[async_trait]
    impl AdminLogService for NoOpAdminLogService {
        async fn log(
            &self,
            _level: AdminLogLevel,
            _category: AdminLogCategory,
            _message: String,
            _details: Option<Value>,
        ) -> Result<()> {
            Ok(())
        }

        async fn get_logs(&self, _limit: u32, _offset: u32) -> Result<Vec<AdminLog>> {
            Ok(vec![])
        }

        async fn get_logs_by_category(
            &self,
            _category: AdminLogCategory,
            _limit: u32,
            _offset: u32,
        ) -> Result<Vec<AdminLog>> {
            Ok(vec![])
        }

        async fn count(&self) -> Result<u64> {
            Ok(0)
        }
    }
}

// Re-exported at the module root so the double keeps the path it had before it
// moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::NoOpAdminLogService;

#[cfg(test)]
mod tests {
    use super::*;
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;

    // The store's ordering and paging are its contract's to prove
    // (`admin_log_repository_contract`); what is the service's own is that it
    // writes an entry through whole, hands the store the page it was asked
    // for, and reports a store failure rather than swallowing it.

    #[tokio::test]
    async fn an_entry_is_stored_whole_and_a_page_is_the_one_asked_for() {
        let repo = Arc::new(InMemoryAdminLogRepository::default());
        let service = LocalAdminLogService::new(repo.clone());
        let details = serde_json::json!({"free_gb": 5});
        for n in 0..4 {
            service
                .log(
                    AdminLogLevel::Warning,
                    AdminLogCategory::System,
                    format!("system {n}"),
                    Some(details.clone()),
                )
                .await
                .unwrap();
        }
        service
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                "scan".to_string(),
                None,
            )
            .await
            .unwrap();

        let stored = repo.list(10, 0).await.unwrap();
        let system = stored
            .iter()
            .find(|entry| entry.message == "system 0")
            .expect("written through to the store");
        assert_eq!(
            (&system.level, &system.category, system.details.as_ref()),
            (
                &AdminLogLevel::Warning,
                &AdminLogCategory::System,
                Some(&details)
            )
        );
        // A limit and an offset that differ, so a swap of the two is seen.
        let ids = |entries: Vec<AdminLog>| -> Vec<uuid::Uuid> {
            entries.into_iter().map(|entry| entry.id).collect()
        };
        let page = ids(service.get_logs(1, 2).await.unwrap());
        assert_eq!(page.len(), 1);
        assert_eq!(page, ids(repo.list(1, 2).await.unwrap()));
        let category_page = ids(service
            .get_logs_by_category(AdminLogCategory::System, 3, 1)
            .await
            .unwrap());
        assert_eq!(category_page.len(), 3);
        assert_eq!(
            category_page,
            ids(repo
                .list_by_category(AdminLogCategory::System, 3, 1)
                .await
                .unwrap())
        );
        assert_eq!(service.count().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn an_administrators_action_is_stored_with_who_did_it() {
        let repo = Arc::new(InMemoryAdminLogRepository::default());
        let service = LocalAdminLogService::new(repo.clone());

        record_admin_action(
            &service,
            AdminLogCategory::System,
            "Created library \"Films\"".to_owned(),
            "admin-1",
            serde_json::json!({"library_id": "lib-1"}),
        )
        .await;
        // A caller that hands over something other than an object keeps it,
        // and the administrator's id is still recorded beside it.
        record_admin_action(
            &service,
            AdminLogCategory::Enrichment,
            "Refreshed".to_owned(),
            "admin-2",
            serde_json::json!(7),
        )
        .await;

        let stored = repo.list(10, 0).await.unwrap();
        let created = stored
            .iter()
            .find(|entry| entry.message == "Created library \"Films\"")
            .expect("written through to the store");
        assert_eq!(
            (&created.level, &created.category, created.details.as_ref()),
            (
                &AdminLogLevel::Info,
                &AdminLogCategory::System,
                Some(&serde_json::json!({"library_id": "lib-1", "admin_user_id": "admin-1"}))
            )
        );
        let refreshed = stored
            .iter()
            .find(|entry| entry.message == "Refreshed")
            .expect("written through to the store");
        assert_eq!(
            (&refreshed.category, refreshed.details.as_ref()),
            (
                &AdminLogCategory::Enrichment,
                Some(&serde_json::json!({"detail": 7, "admin_user_id": "admin-2"}))
            )
        );
    }

    #[tokio::test]
    async fn a_recorded_event_keeps_its_level() {
        let repo = Arc::new(InMemoryAdminLogRepository::default());
        let service = LocalAdminLogService::new(repo.clone());
        for (level, message) in [
            (AdminLogLevel::Info, "info"),
            (AdminLogLevel::Warning, "warning"),
            (AdminLogLevel::Error, "error"),
        ] {
            record(
                &service,
                level,
                AdminLogCategory::Auth,
                message.to_owned(),
                serde_json::json!({}),
            )
            .await;
        }
        let mut stored: Vec<(String, AdminLogLevel)> = repo
            .list(10, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| (entry.message, entry.level))
            .collect();
        stored.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            stored,
            vec![
                ("error".to_owned(), AdminLogLevel::Error),
                ("info".to_owned(), AdminLogLevel::Info),
                ("warning".to_owned(), AdminLogLevel::Warning),
            ]
        );
    }

    /// A store that fails every call.
    #[derive(Debug)]
    struct UnavailableStore;

    fn unavailable() -> sea_orm::DbErr {
        sea_orm::DbErr::Custom("the database is down".to_string())
    }

    #[async_trait]
    impl AdminLogRepository for UnavailableStore {
        async fn create(
            &self,
            _entry: CreateAdminLog,
        ) -> std::result::Result<AdminLog, sea_orm::DbErr> {
            Err(unavailable())
        }

        async fn list(
            &self,
            _limit: u64,
            _offset: u64,
        ) -> std::result::Result<Vec<AdminLog>, sea_orm::DbErr> {
            Err(unavailable())
        }

        async fn list_by_category(
            &self,
            _category: AdminLogCategory,
            _limit: u64,
            _offset: u64,
        ) -> std::result::Result<Vec<AdminLog>, sea_orm::DbErr> {
            Err(unavailable())
        }

        async fn count(&self) -> std::result::Result<u64, sea_orm::DbErr> {
            Err(unavailable())
        }
    }

    /// NFR-205: a store failure reaches the caller as a database error, on
    /// every operation.
    #[tokio::test]
    async fn a_store_failure_is_reported_on_every_operation() {
        let service = LocalAdminLogService::new(Arc::new(UnavailableStore));
        let is_database = |err: AdminLogError| match err {
            AdminLogError::Database(message) => message.contains("the database is down"),
        };

        let logged = service
            .log(
                AdminLogLevel::Error,
                AdminLogCategory::Auth,
                "denied".to_string(),
                None,
            )
            .await;
        assert!(logged.is_err_and(is_database));
        assert!(service.get_logs(10, 0).await.is_err_and(is_database));
        assert!(
            service
                .get_logs_by_category(AdminLogCategory::Auth, 10, 0)
                .await
                .is_err_and(is_database)
        );
        assert!(service.count().await.is_err_and(is_database));
    }
}
