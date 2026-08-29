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

    #[tokio::test]
    async fn test_log_and_retrieve() {
        let repo = Arc::new(InMemoryAdminLogRepository::default());
        let service = LocalAdminLogService::new(repo);

        service
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::LibraryScan,
                "Scan started".to_string(),
                None,
            )
            .await
            .unwrap();

        service
            .log(
                AdminLogLevel::Warning,
                AdminLogCategory::System,
                "Disk space low".to_string(),
                Some(serde_json::json!({"free_gb": 5})),
            )
            .await
            .unwrap();

        let logs = service.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 2);
        // Most recent first
        assert_eq!(logs[0].message, "Disk space low");
        assert_eq!(logs[1].message, "Scan started");

        let count = service.count().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_noop_service() {
        let service = NoOpAdminLogService;
        service
            .log(
                AdminLogLevel::Error,
                AdminLogCategory::Auth,
                "Should not panic".to_string(),
                None,
            )
            .await
            .unwrap();
        let logs = service.get_logs(10, 0).await.unwrap();
        assert!(logs.is_empty());
    }
}
