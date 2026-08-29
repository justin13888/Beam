use async_trait::async_trait;
use sea_orm::DbErr;

use crate::models::admin_log::{AdminLog, AdminLogCategory, CreateAdminLog};

#[async_trait]
pub trait AdminLogRepository: Send + Sync + std::fmt::Debug {
    async fn create(&self, entry: CreateAdminLog) -> Result<AdminLog, DbErr>;
    async fn list(&self, limit: u64, offset: u64) -> Result<Vec<AdminLog>, DbErr>;
    /// Like [`list`](Self::list) (newest first) but restricted to a single
    /// category -- e.g. `LibraryScan` for the admin status endpoint's recent
    /// scan history (issue #85).
    async fn list_by_category(
        &self,
        category: AdminLogCategory,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<AdminLog>, DbErr>;
    async fn count(&self) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use uuid::Uuid;

    #[derive(Debug, Default)]
    pub struct InMemoryAdminLogRepository {
        logs: Arc<RwLock<Vec<AdminLog>>>,
    }

    #[async_trait]
    impl AdminLogRepository for InMemoryAdminLogRepository {
        async fn create(&self, entry: CreateAdminLog) -> Result<AdminLog, DbErr> {
            let log = AdminLog {
                id: Uuid::new_v4(),
                level: entry.level,
                category: entry.category,
                message: entry.message,
                details: entry.details,
                created_at: chrono::Utc::now(),
            };
            self.logs.write().push(log.clone());
            Ok(log)
        }

        async fn list(&self, limit: u64, offset: u64) -> Result<Vec<AdminLog>, DbErr> {
            let logs = self.logs.read();
            let mut sorted: Vec<AdminLog> = logs.clone();
            sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            let start = offset as usize;
            let end = (offset + limit) as usize;
            Ok(sorted.into_iter().skip(start).take(end - start).collect())
        }

        async fn list_by_category(
            &self,
            category: AdminLogCategory,
            limit: u64,
            offset: u64,
        ) -> Result<Vec<AdminLog>, DbErr> {
            let logs = self.logs.read();
            let mut matching: Vec<AdminLog> = logs
                .iter()
                .filter(|log| log.category == category)
                .cloned()
                .collect();
            matching.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Ok(matching
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        }

        async fn count(&self) -> Result<u64, DbErr> {
            Ok(self.logs.read().len() as u64)
        }
    }
}
