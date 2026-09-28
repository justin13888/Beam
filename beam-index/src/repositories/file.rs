use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::{
    CreateMediaFile, FileClassification, FileRelink, MediaFile, MediaFileContent, ProbeUpdate,
    UpdateMediaFile, displaced_path,
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

/// The `LIKE` pattern, escaped with `\`, that matches a stored path strictly
/// beneath the directory `dir`: its text, wildcards escaped, then a
/// separator and anything. The separator keeps `/a/S1` from matching
/// `/a/S10/x.mkv`, and escaping keeps a `_` or `%` in a directory's name
/// from matching any character.
fn beneath_pattern(dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    let dir = dir.trim_end_matches(std::path::MAIN_SEPARATOR);
    let mut pattern = String::with_capacity(dir.len() + 2);
    for c in dir.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push(std::path::MAIN_SEPARATOR);
    pattern.push('%');
    pattern
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

    /// A reconcile read: no `missing_since` filter. Served by
    /// `idx_files_hash`.
    async fn find_by_library_and_hash_including_missing(
        &self,
        library_id: Uuid,
        hash: u64,
    ) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::HashXxh3.eq(hash as i64))
            .filter(files::Column::LibraryId.eq(library_id))
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_beneath_including_missing(
        &self,
        library_id: Uuid,
        dir: &Path,
    ) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::sea_query::LikeExpr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .filter(files::Column::FilePath.like(LikeExpr::new(beneath_pattern(dir)).escape('\\')))
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
            } => {
                active_model.mime_type = Set(Some(mime_type));
                active_model.duration_secs = Set(Some(duration.as_secs_f64()));
                active_model.container_format = Set(Some(container_format));
            }
            ProbeUpdate::Clear => {
                active_model.mime_type = Set(None);
                active_model.duration_secs = Set(None);
                active_model.container_format = Set(None);
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

    async fn relink(
        &self,
        relinks: Vec<FileRelink>,
        displaced: Vec<Uuid>,
        at: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        use beam_entity::files;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

        if relinks.is_empty() && displaced.is_empty() {
            return Ok(());
        }
        let mut named = std::collections::HashSet::new();
        for id in relinks
            .iter()
            .map(|relink| relink.id)
            .chain(displaced.iter().copied())
        {
            if !named.insert(id) {
                return Err(DbErr::Custom(format!("file {id} is named twice")));
            }
        }
        // `idx_files_path_unique` is checked per statement, so rows trading
        // paths cannot be written straight to their new ones: each relinked
        // row first steps aside to a path only it can hold, and only then
        // takes its new one. A path held by a row outside the call, or named
        // twice, still fails the unique index, and the transaction -- rolled
        // back when dropped unfinished -- leaves every row as it was.
        let txn = self.db.begin().await?;
        let now: DateTimeWithTimeZone = chrono::Utc::now().into();
        let stored = |id: Uuid| {
            let txn = &txn;
            async move {
                files::Entity::find_by_id(id)
                    .one(txn)
                    .await?
                    .ok_or_else(|| DbErr::RecordNotFound(format!("File {id} not found")))
            }
        };
        for id in displaced {
            let row = stored(id).await?;
            let parked = displaced_path(std::path::Path::new(&row.file_path), id);
            files::ActiveModel {
                id: Set(id),
                file_path: Set(parked.to_string_lossy().to_string()),
                missing_since: Set(Some(row.missing_since.unwrap_or_else(|| at.into()))),
                updated_at: Set(now),
                ..Default::default()
            }
            .update(&txn)
            .await?;
        }
        for relink in &relinks {
            let row = stored(relink.id).await?;
            files::ActiveModel {
                id: Set(relink.id),
                file_path: Set(format!("{}.beam-relinking-{}", row.file_path, relink.id)),
                ..Default::default()
            }
            .update(&txn)
            .await?;
        }
        for FileRelink {
            id,
            path,
            size_bytes,
            mtime,
        } in relinks
        {
            files::ActiveModel {
                id: Set(id),
                file_path: Set(path.to_string_lossy().to_string()),
                file_size: Set(size_bytes as i64),
                mtime: Set(mtime.map(|d| d.into())),
                missing_since: Set(None),
                updated_at: Set(now),
                ..Default::default()
            }
            .update(&txn)
            .await?;
        }
        txn.commit().await
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
