use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::file::container_tags_json;
use beam_domain::models::{
    CreateMediaFile, FileClassification, MediaFile, MediaFileContent, ProbeUpdate, UpdateMediaFile,
};

/// The `files` columns a file's content is stored in: `(movie_entry_id,
/// episode_id, last_episode_number)`. Exactly one of the first two is set for
/// classified content, neither for none.
fn content_columns(content: Option<MediaFileContent>) -> (Option<Uuid>, Option<Uuid>, Option<i32>) {
    match content {
        Some(MediaFileContent::Movie { movie_entry_id }) => (Some(movie_entry_id), None, None),
        Some(MediaFileContent::Episode {
            episode_id,
            last_episode_number,
        }) => (
            None,
            Some(episode_id),
            last_episode_number.map(|n| n as i32),
        ),
        None => (None, None, None),
    }
}
use beam_domain::repositories::FileRepository;

/// SQL-based implementation of the FileRepository trait.
#[derive(Debug, Clone)]
pub struct SqlFileRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlFileRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl FileRepository for SqlFileRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::EntityTrait;

        use sea_orm::{ColumnTrait, QueryFilter};

        let model = files::Entity::find_by_id(id)
            .filter(files::Column::MissingSince.is_null())
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(MediaFile::from))
    }

    /// A reconcile read: deliberately no `missing_since` filter.
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = files::Entity::find()
            .filter(files::Column::FilePath.eq(path))
            .one(self.db.as_ref())
            .await?;

        Ok(model.map(MediaFile::from))
    }

    async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::HashXxh3.eq(hash as i64))
            .filter(files::Column::MissingSince.is_null())
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .filter(files::Column::MissingSince.is_null())
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_all_by_library_including_missing(
        &self,
        library_id: Uuid,
    ) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_all_under(&self, library_id: Uuid, dir: &Path) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // `starts_with` rather than `LIKE`: the directory is a literal, and a
        // `_` or `%` in a folder name is not a wildcard. The separator makes
        // the prefix match whole components only.
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        let models = files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .filter(files::Column::MissingSince.is_null())
            .filter(Expr::cust_with_values(
                r#"starts_with("files"."file_path", $1)"#,
                [prefix],
            ))
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::MovieEntryId.eq(movie_entry_id))
            .filter(files::Column::MissingSince.is_null())
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::EpisodeId.eq(episode_id))
            .filter(files::Column::MissingSince.is_null())
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr> {
        use beam_entity::files;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let (movie_entry_id, episode_id, last_episode_number) = content_columns(create.content);

        let new_file = files::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            library_id: Set(create.library_id),
            file_path: Set(create.path.to_string_lossy().to_string()),
            hash_xxh3: Set(create.hash as i64),
            file_size: Set(create.size_bytes as i64),
            mime_type: Set(create.mime_type),
            duration_secs: Set(create.duration.map(|d| d.as_secs_f64())),
            container_format: Set(create.container_format),
            language: Set(None),
            quality: Set(None),
            release_group: Set(None),
            is_primary: Set(true),
            movie_entry_id: Set(movie_entry_id),
            episode_id: Set(episode_id),
            scanned_at: Set(now.into()),
            updated_at: Set(now.into()),
            file_status: Set(create.status.into()),
            mtime: Set(create.mtime.map(|d| d.into())),
            missing_since: Set(None),
            last_episode_number: Set(last_episode_number),
            classifier_version: Set(create.classifier_version as i16),
            container_tags: Set(create.container_tags.as_ref().map(container_tags_json)),
        };

        let result = new_file.insert(self.db.as_ref()).await?;
        Ok(MediaFile::from(result))
    }

    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
        use beam_entity::files;
        use sea_orm::{ActiveModelTrait, Set};

        let mut active_model: files::ActiveModel = files::ActiveModel {
            id: Set(update.id),
            ..Default::default()
        };

        if let Some(hash) = update.hash {
            active_model.hash_xxh3 = Set(hash as i64);
        }
        if let Some(size) = update.size_bytes {
            active_model.file_size = Set(size as i64);
        }
        if let Some(mtime) = update.mtime {
            active_model.mtime = Set(Some(mtime.into()));
        }
        match update.probe {
            ProbeUpdate::Keep => {}
            ProbeUpdate::Set {
                mime_type,
                duration,
                container_format,
                container_tags,
            } => {
                active_model.mime_type = Set(Some(mime_type));
                active_model.duration_secs = Set(Some(duration.as_secs_f64()));
                active_model.container_format = Set(Some(container_format));
                active_model.container_tags = Set(Some(container_tags_json(&container_tags)));
            }
            ProbeUpdate::Clear => {
                active_model.mime_type = Set(None);
                active_model.duration_secs = Set(None);
                active_model.container_format = Set(None);
                active_model.container_tags = Set(None);
            }
        }
        if let Some(status) = update.status {
            active_model.file_status = Set(status.into());
        }

        if let Some(content) = update.content {
            let (movie_entry_id, episode_id, last_episode_number) = content_columns(Some(content));
            active_model.movie_entry_id = Set(movie_entry_id);
            active_model.episode_id = Set(episode_id);
            active_model.last_episode_number = Set(last_episode_number);
        }

        active_model.updated_at = Set(chrono::Utc::now().into());

        let result = active_model.update(self.db.as_ref()).await?;
        Ok(MediaFile::from(result))
    }

    async fn set_classification(
        &self,
        id: Uuid,
        classification: FileClassification,
    ) -> Result<MediaFile, DbErr> {
        use beam_entity::files;
        use sea_orm::{ActiveModelTrait, Set};

        let FileClassification {
            content,
            status,
            classifier_version,
        } = classification;
        let (movie_entry_id, episode_id, last_episode_number) = content_columns(content);
        let result = files::ActiveModel {
            id: Set(id),
            movie_entry_id: Set(movie_entry_id),
            episode_id: Set(episode_id),
            last_episode_number: Set(last_episode_number),
            file_status: Set(status.into()),
            classifier_version: Set(classifier_version as i16),
            updated_at: Set(chrono::Utc::now().into()),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await?;
        Ok(MediaFile::from(result))
    }

    async fn mark_missing(&self, ids: Vec<Uuid>, at: DateTime<Utc>) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(0);
        }

        // `missing_since IS NULL` keeps the first stamp on a row that is
        // already missing: the grace period runs from when the file was first
        // found gone.
        let stamp: DateTimeWithTimeZone = at.into();
        let result = files::Entity::update_many()
            .col_expr(files::Column::MissingSince, Expr::value(stamp))
            .filter(files::Column::Id.is_in(ids))
            .filter(files::Column::MissingSince.is_null())
            .exec(self.db.as_ref())
            .await?;

        Ok(result.rows_affected)
    }

    async fn restore(&self, id: Uuid) -> Result<(), DbErr> {
        use beam_entity::files;
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        files::Entity::update_many()
            .col_expr(
                files::Column::MissingSince,
                Expr::value(Option::<DateTimeWithTimeZone>::None),
            )
            .filter(files::Column::Id.eq(id))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn purge_missing(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(0);
        }

        // The `IS NOT NULL` guard is what makes this the only hard delete a
        // present file can never reach: a row restored after the caller read
        // it is skipped rather than purged.
        let result = files::Entity::delete_many()
            .filter(files::Column::Id.is_in(ids))
            .filter(files::Column::MissingSince.is_not_null())
            .exec(self.db.as_ref())
            .await?;

        Ok(result.rows_affected)
    }

    async fn count_all(&self) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

        files::Entity::find()
            .filter(files::Column::MissingSince.is_null())
            .count(self.db.as_ref())
            .await
    }
}
