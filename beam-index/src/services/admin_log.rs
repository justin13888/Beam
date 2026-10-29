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
