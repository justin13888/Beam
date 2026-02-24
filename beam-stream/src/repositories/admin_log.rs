use async_trait::async_trait;
use sea_orm::*;
use uuid::Uuid;

use crate::models::domain::{AdminLog, AdminLogCategory, AdminLogLevel, CreateAdminLog};

#[async_trait]
pub trait AdminLogRepository: Send + Sync + std::fmt::Debug {
    async fn create(&self, entry: CreateAdminLog) -> Result<AdminLog, DbErr>;
    async fn list(&self, limit: u64, offset: u64) -> Result<Vec<AdminLog>, DbErr>;
    async fn count(&self) -> Result<u64, DbErr>;
}

#[derive(Debug)]
pub struct SqlAdminLogRepository {
    db: DatabaseConnection,
}

impl SqlAdminLogRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AdminLogRepository for SqlAdminLogRepository {
    async fn create(&self, entry: CreateAdminLog) -> Result<AdminLog, DbErr> {
        use beam_entity::admin_log;
        use sea_orm::ActiveValue::Set;

        let model = admin_log::ActiveModel {
            id: Set(Uuid::new_v4()),
            level: Set(entry.level.into()),
            category: Set(entry.category.into()),
            message: Set(entry.message),
            details: Set(entry.details),
            created_at: Set(chrono::Utc::now().into()),
        };

        let result = admin_log::Entity::insert(model)
            .exec_with_returning(&self.db)
            .await?;

        Ok(AdminLog::from(result))
    }

    async fn list(&self, limit: u64, offset: u64) -> Result<Vec<AdminLog>, DbErr> {
        use beam_entity::admin_log;
        use sea_orm::{EntityTrait, QueryOrder, QuerySelect};

        let models = admin_log::Entity::find()
            .order_by_desc(admin_log::Column::CreatedAt)
            .limit(limit)
            .offset(offset)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(AdminLog::from).collect())
    }

    async fn count(&self) -> Result<u64, DbErr> {
        use beam_entity::admin_log;
        use sea_orm::EntityTrait;

        admin_log::Entity::find().count(&self.db).await
    }
}

/// In-memory implementation for use in tests
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::sync::Arc;

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

        async fn count(&self) -> Result<u64, DbErr> {
            Ok(self.logs.read().len() as u64)
        }
    }
}
