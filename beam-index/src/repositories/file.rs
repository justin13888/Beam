use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};

use crate::models::domain::{CreateMediaFile, MediaFile, UpdateMediaFile};
use uuid::Uuid;

/// Repository for managing media file persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait FileRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr>;
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr>;
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr>;
    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr>;
    async fn delete(&self, id: Uuid) -> Result<(), DbErr>;
    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr>;
}

/// SQL-based implementation of the FileRepository trait.
#[derive(Debug, Clone)]
pub struct SqlFileRepository {
    db: DatabaseConnection,
}

impl SqlFileRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl FileRepository for SqlFileRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::EntityTrait;

        let model = files::Entity::find_by_id(id).one(&self.db).await?;
        Ok(model.map(MediaFile::from))
    }

    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = files::Entity::find()
            .filter(files::Column::FilePath.eq(path))
            .one(&self.db)
            .await?;

        Ok(model.map(MediaFile::from))
    }

    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::LibraryId.eq(library_id))
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::MovieEntryId.eq(movie_entry_id))
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let models = files::Entity::find()
            .filter(files::Column::EpisodeId.eq(episode_id))
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(MediaFile::from).collect())
    }

    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr> {
        use crate::models::domain::MediaFileContent;
        use beam_entity::files;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let (movie_entry_id, episode_id) = match create.content {
            Some(MediaFileContent::Movie { movie_entry_id }) => (Some(movie_entry_id), None),
            Some(MediaFileContent::Episode { episode_id }) => (None, Some(episode_id)),
            None => (None, None),
        };

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
            file_status: Set(create.status.to_string()),
        };

        let result = new_file.insert(&self.db).await?;
        Ok(MediaFile::from(result))
    }

    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
        use crate::models::domain::MediaFileContent;
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
        if let Some(mime_type) = update.mime_type {
            active_model.mime_type = Set(Some(mime_type));
        }
        if let Some(duration) = update.duration {
            active_model.duration_secs = Set(Some(duration.as_secs_f64()));
        }
        if let Some(container) = update.container_format {
            active_model.container_format = Set(Some(container));
        }
        if let Some(status) = update.status {
            active_model.file_status = Set(status.to_string());
        }

        // Handle content update
        if let Some(content) = update.content {
            match content {
                MediaFileContent::Movie { movie_entry_id } => {
                    active_model.movie_entry_id = Set(Some(movie_entry_id));
                    active_model.episode_id = Set(None);
                }
                MediaFileContent::Episode { episode_id } => {
                    active_model.movie_entry_id = Set(None);
                    active_model.episode_id = Set(Some(episode_id));
                }
            }
        }

        // Also update timestamp
        active_model.updated_at = Set(chrono::Utc::now().into());

        let result = active_model.update(&self.db).await?;
        Ok(MediaFile::from(result))
    }

    async fn delete(&self, id: Uuid) -> Result<(), DbErr> {
        use beam_entity::files;
        use sea_orm::EntityTrait;

        files::Entity::delete_by_id(id).exec(&self.db).await?;
        Ok(())
    }

    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        use beam_entity::files;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if ids.is_empty() {
            return Ok(0);
        }

        let result = files::Entity::delete_many()
            .filter(files::Column::Id.is_in(ids))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected)
    }
}

/// In-memory implementation for use in tests and test-utils consumers.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryFileRepository {
        pub files: Mutex<HashMap<Uuid, MediaFile>>,
    }

    #[async_trait]
    impl FileRepository for InMemoryFileRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
            Ok(self.files.lock().unwrap().get(&id).cloned())
        }

        async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .find(|f| f.path == Path::new(path))
                .cloned())
        }

        async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| f.library_id == library_id)
                .cloned()
                .collect())
        }

        async fn find_by_movie_entry_id(
            &self,
            movie_entry_id: Uuid,
        ) -> Result<Vec<MediaFile>, DbErr> {
            use crate::models::domain::MediaFileContent;
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| {
                    matches!(&f.content, Some(MediaFileContent::Movie { movie_entry_id: id }) if *id == movie_entry_id)
                })
                .cloned()
                .collect())
        }

        async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
            use crate::models::domain::MediaFileContent;
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| {
                    matches!(&f.content, Some(MediaFileContent::Episode { episode_id: id }) if *id == episode_id)
                })
                .cloned()
                .collect())
        }

        async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr> {
            let file = MediaFile {
                id: Uuid::new_v4(),
                library_id: create.library_id,
                path: create.path,
                hash: create.hash,
                size_bytes: create.size_bytes,
                mime_type: create.mime_type,
                duration: create.duration,
                container_format: create.container_format,
                content: create.content,
                status: create.status,
                scanned_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.files.lock().unwrap().insert(file.id, file.clone());
            Ok(file)
        }

        async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
            let mut files = self.files.lock().unwrap();
            let file = files
                .get_mut(&update.id)
                .ok_or(DbErr::RecordNotFound(format!(
                    "File {} not found",
                    update.id
                )))?;
            if let Some(status) = update.status {
                file.status = status;
            }
            Ok(file.clone())
        }

        async fn delete(&self, id: Uuid) -> Result<(), DbErr> {
            self.files.lock().unwrap().remove(&id);
            Ok(())
        }

        async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
            let mut files = self.files.lock().unwrap();
            let mut count = 0u64;
            for id in ids {
                if files.remove(&id).is_some() {
                    count += 1;
                }
            }
            Ok(count)
        }
    }
}
