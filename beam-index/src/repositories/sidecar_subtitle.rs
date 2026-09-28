use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::sidecar::{SidecarInfo, SidecarSubtitle, UpsertSidecarSubtitle};
use beam_domain::repositories::SidecarSubtitleRepository;

/// SQL implementation of [`SidecarSubtitleRepository`] over the
/// `sidecar_subtitles` table.
#[derive(Debug, Clone)]
pub struct SqlSidecarSubtitleRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlSidecarSubtitleRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn rows(models: Vec<beam_entity::sidecar_subtitle::Model>) -> Result<Vec<SidecarSubtitle>, DbErr> {
    models.into_iter().map(SidecarSubtitle::try_from).collect()
}

#[async_trait]
impl SidecarSubtitleRepository for SqlSidecarSubtitleRepository {
    async fn upsert_by_path(
        &self,
        upsert: UpsertSidecarSubtitle,
    ) -> Result<SidecarSubtitle, DbErr> {
        use beam_entity::sidecar_subtitle;
        use sea_orm::sea_query::OnConflict;
        use sea_orm::{EntityTrait, Set};

        let UpsertSidecarSubtitle {
            file_id,
            library_id,
            path,
            info,
            size_bytes,
            mtime,
        } = upsert;
        let SidecarInfo {
            format,
            language,
            title,
            is_forced,
            is_sdh,
            is_default,
        } = info;
        let path = path_text(&path);
        let now = Utc::now();
        let active = sidecar_subtitle::ActiveModel {
            id: Set(Uuid::new_v4()),
            file_id: Set(file_id),
            library_id: Set(library_id),
            path: Set(path.clone()),
            format: Set(format.as_str().to_string()),
            language: Set(language),
            title: Set(title),
            is_forced: Set(is_forced),
            is_sdh: Set(is_sdh),
            is_default: Set(is_default),
            size_bytes: Set(i64::try_from(size_bytes).unwrap_or(i64::MAX)),
            mtime: Set(mtime.map(Into::into)),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };
        // One `INSERT ... ON CONFLICT (path) DO UPDATE`: a scan and a watcher
        // event for one subtitle cannot both insert. The row at the path keeps
        // its id and `created_at`.
        sidecar_subtitle::Entity::insert(active)
            .on_conflict(
                OnConflict::column(sidecar_subtitle::Column::Path)
                    .update_columns([
                        sidecar_subtitle::Column::FileId,
                        sidecar_subtitle::Column::LibraryId,
                        sidecar_subtitle::Column::Format,
                        sidecar_subtitle::Column::Language,
                        sidecar_subtitle::Column::Title,
                        sidecar_subtitle::Column::IsForced,
                        sidecar_subtitle::Column::IsSdh,
                        sidecar_subtitle::Column::IsDefault,
                        sidecar_subtitle::Column::SizeBytes,
                        sidecar_subtitle::Column::Mtime,
                        sidecar_subtitle::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;

        self.find_by_path(Path::new(&path)).await?.ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "sidecar subtitle {path:?} is not readable after its upsert"
            ))
        })
    }

    async fn find_by_path(&self, path: &Path) -> Result<Option<SidecarSubtitle>, DbErr> {
        use beam_entity::sidecar_subtitle;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        sidecar_subtitle::Entity::find()
            .filter(sidecar_subtitle::Column::Path.eq(path_text(path)))
            .one(self.db.as_ref())
            .await?
            .map(SidecarSubtitle::try_from)
            .transpose()
    }

    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr> {
        use beam_entity::sidecar_subtitle;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        rows(
            sidecar_subtitle::Entity::find()
                .filter(sidecar_subtitle::Column::FileId.eq(file_id))
                .order_by_asc(sidecar_subtitle::Column::Path)
                .all(self.db.as_ref())
                .await?,
        )
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<SidecarSubtitle>, DbErr> {
        use beam_entity::sidecar_subtitle;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        rows(
            sidecar_subtitle::Entity::find()
                .filter(sidecar_subtitle::Column::LibraryId.eq(library_id))
                .order_by_asc(sidecar_subtitle::Column::Path)
                .all(self.db.as_ref())
                .await?,
        )
    }

    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        use beam_entity::sidecar_subtitle;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(0);
        }
        let result = sidecar_subtitle::Entity::delete_many()
            .filter(sidecar_subtitle::Column::Id.is_in(ids))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }
}
