use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::*;
use uuid::Uuid;

use beam_domain::models::{AdminLog, AdminLogCategory, CreateAdminLog};
use beam_domain::repositories::AdminLogRepository;

#[derive(Debug)]
pub struct SqlAdminLogRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlAdminLogRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
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
            .exec_with_returning(self.db.as_ref())
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
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(AdminLog::from).collect())
    }

    async fn list_by_category(
        &self,
        category: AdminLogCategory,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<AdminLog>, DbErr> {
        use beam_entity::admin_log;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

        let entity_category: admin_log::AdminLogCategory = category.into();
        let models = admin_log::Entity::find()
            .filter(admin_log::Column::Category.eq(entity_category))
            .order_by_desc(admin_log::Column::CreatedAt)
            .limit(limit)
            .offset(offset)
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(AdminLog::from).collect())
    }

    async fn count(&self) -> Result<u64, DbErr> {
        use beam_entity::admin_log;
        use sea_orm::EntityTrait;

        admin_log::Entity::find().count(self.db.as_ref()).await
    }
}
