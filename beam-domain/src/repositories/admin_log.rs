use async_trait::async_trait;
use sea_orm::DbErr;

use crate::models::admin_log::{AdminLog, AdminLogCategory, CreateAdminLog};

/// The administrator-facing event log: append-only, read newest first.
///
/// Every method returns the store's own failure as `Err(DbErr)`.
#[async_trait]
pub trait AdminLogRepository: Send + Sync + std::fmt::Debug {
    /// Appends `entry` and returns it as stored: a fresh id, stamped from
    /// the store's clock.
    async fn create(&self, entry: CreateAdminLog) -> Result<AdminLog, DbErr>;
    /// At most `limit` entries, newest first, after skipping the `offset`
    /// newest; empty once `offset` passes the end.
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
    /// How many entries the log holds, of every category.
    async fn count(&self) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::services::{Clock, RealClock};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use uuid::Uuid;

    #[derive(Debug)]
    pub struct InMemoryAdminLogRepository {
        logs: Arc<RwLock<Vec<AdminLog>>>,
        /// Source of `created_at`, as the SQL implementation's is.
        clock: Arc<dyn Clock>,
    }

    impl Default for InMemoryAdminLogRepository {
        fn default() -> Self {
            Self::with_clock(Arc::new(RealClock))
        }
    }

    impl InMemoryAdminLogRepository {
        /// A repository stamping `created_at` from `clock`.
        #[must_use]
        pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
            Self {
                logs: Arc::default(),
                clock,
            }
        }
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
                created_at: self.clock.now(),
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

#[cfg(test)]
mod contract_over_in_memory {
    use std::sync::Arc;

    use super::AdminLogRepository;
    use super::in_memory::InMemoryAdminLogRepository;
    use crate::services::TestClock;

    struct InMemoryFixture {
        repo: InMemoryAdminLogRepository,
        clock: Arc<TestClock>,
    }

    impl AdminLogFixture for InMemoryFixture {
        fn repo(&self) -> &dyn AdminLogRepository {
            &self.repo
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }
    }

    async fn setup() -> InMemoryFixture {
        let clock = Arc::new(TestClock::starting_at(
            chrono::DateTime::from_timestamp(1_900_000_000, 0).unwrap(),
        ));
        InMemoryFixture {
            repo: InMemoryAdminLogRepository::with_clock(clock.clone()),
            clock,
        }
    }

    crate::admin_log_repository_contract!(setup);
}
