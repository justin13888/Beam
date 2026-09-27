use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};
use beam_domain::repositories::AppliedNfoRepository;

/// SQL implementation of [`AppliedNfoRepository`] over the `applied_nfos`
/// table.
#[derive(Debug, Clone)]
pub struct SqlAppliedNfoRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlAppliedNfoRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[async_trait]
impl AppliedNfoRepository for SqlAppliedNfoRepository {
    async fn record_by_path(&self, record: RecordAppliedNfo) -> Result<AppliedNfo, DbErr> {
        use beam_entity::applied_nfo;
        use sea_orm::sea_query::OnConflict;
        use sea_orm::{EntityTrait, Set};

        let RecordAppliedNfo {
            library_id,
            path,
            size_bytes,
            content_hash,
            change_stamp,
        } = record;
        let path = path_text(&path);
        let now = Utc::now();
        let active = applied_nfo::ActiveModel {
            id: Set(Uuid::new_v4()),
            library_id: Set(library_id),
            path: Set(path.clone()),
            size_bytes: Set(i64::try_from(size_bytes).unwrap_or(i64::MAX)),
            content_hash: Set(content_hash),
            change_stamp: Set(change_stamp),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };
        // One `INSERT ... ON CONFLICT (path) DO UPDATE`: a scan and a watcher
        // event for one NFO cannot both insert. The row at the path keeps its
        // id and `created_at`.
        applied_nfo::Entity::insert(active)
            .on_conflict(
                OnConflict::column(applied_nfo::Column::Path)
                    .update_columns([
                        applied_nfo::Column::LibraryId,
                        applied_nfo::Column::SizeBytes,
                        applied_nfo::Column::ContentHash,
                        applied_nfo::Column::ChangeStamp,
                        applied_nfo::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;

        self.find_by_path(Path::new(&path)).await?.ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "applied NFO {path:?} is not readable after its record"
            ))
        })
    }

    async fn find_by_path(&self, path: &Path) -> Result<Option<AppliedNfo>, DbErr> {
        use beam_entity::applied_nfo;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        Ok(applied_nfo::Entity::find()
            .filter(applied_nfo::Column::Path.eq(path_text(path)))
            .one(self.db.as_ref())
            .await?
            .map(AppliedNfo::from))
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<AppliedNfo>, DbErr> {
        use beam_entity::applied_nfo;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        Ok(applied_nfo::Entity::find()
            .filter(applied_nfo::Column::LibraryId.eq(library_id))
            .order_by_asc(applied_nfo::Column::Path)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(AppliedNfo::from)
            .collect())
    }

    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        use beam_entity::applied_nfo;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(0);
        }
        let result = applied_nfo::Entity::delete_many()
            .filter(applied_nfo::Column::Id.is_in(ids))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }
}
