use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{CreateLibrary, Library};
use beam_domain::repositories::LibraryRepository;

/// SQL-based implementation of the LibraryRepository trait.
#[derive(Debug, Clone)]
pub struct SqlLibraryRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlLibraryRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl LibraryRepository for SqlLibraryRepository {
    async fn find_all(&self) -> Result<Vec<Library>, DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        let models = library::Entity::find().all(self.db.as_ref()).await?;
        Ok(models.into_iter().map(Library::from).collect())
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>, DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        let model = library::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(Library::from))
    }

    async fn create(&self, create: CreateLibrary) -> Result<Library, DbErr> {
        use beam_entity::library;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let new_library = library::ActiveModel {
            id: Set(Uuid::new_v4()),
            name: Set(create.name),
            root_path: Set(create.root_path.to_string_lossy().to_string()),
            description: Set(create.description),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            last_scan_started_at: Set(None),
            last_scan_finished_at: Set(None),
            last_scan_file_count: Set(None),
        };

        let result = new_library.insert(self.db.as_ref()).await?;
        Ok(Library::from(result))
    }

    async fn count_files(&self, library_id: Uuid) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        // Present files only: a missing file (issue #179) is not part of the
        // library a user can see, so it is not in the count a scan records.
        files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .filter(files::Column::MissingSince.is_null())
            .count(self.db.as_ref())
            .await
    }

    async fn update_scan_progress(
        &self,
        library_id: Uuid,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        file_count: Option<i32>,
    ) -> Result<(), DbErr> {
        use beam_entity::library;
        use sea_orm::prelude::DateTimeWithTimeZone;
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // One `UPDATE .. WHERE id = $1` of only the fields given, so an
        // unknown id -- a library deleted while its scan ran -- is a no-op,
        // as it is for every other implementation, rather than a
        // `RecordNotUpdated` (issue #349). With no field given there is
        // nothing to write.
        let mut update = library::Entity::update_many().filter(library::Column::Id.eq(library_id));
        let mut any = false;
        if let Some(started) = started_at {
            update = update.col_expr(
                library::Column::LastScanStartedAt,
                Expr::value(Some(DateTimeWithTimeZone::from(started))),
            );
            any = true;
        }
        if let Some(finished) = finished_at {
            update = update.col_expr(
                library::Column::LastScanFinishedAt,
                Expr::value(Some(DateTimeWithTimeZone::from(finished))),
            );
            any = true;
        }
        if let Some(count) = file_count {
            update = update.col_expr(library::Column::LastScanFileCount, Expr::value(Some(count)));
            any = true;
        }
        if any {
            update.exec(self.db.as_ref()).await?;
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<(), DbErr> {
        use beam_entity::library;
        use sea_orm::EntityTrait;

        library::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn count(&self) -> Result<u64, DbErr> {
        use beam_entity::library;
        use sea_orm::{EntityTrait, PaginatorTrait};

        library::Entity::find().count(self.db.as_ref()).await
    }
}
