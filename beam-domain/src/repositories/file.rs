use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::file::{CreateMediaFile, MediaFile, UpdateMediaFile};

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait FileRepository: Send + Sync + std::fmt::Debug {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr>;
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr>;
    /// Find all files whose content hash matches, across every library.
    /// Used for duplicate detection.
    async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr>;
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr>;
    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr>;
    async fn delete(&self, id: Uuid) -> Result<(), DbErr>;
    async fn delete_by_ids(&self, ids: Vec<Uuid>) -> Result<u64, DbErr>;
}

#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::MediaFileContent;
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

        async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| f.hash == hash)
                .cloned()
                .collect())
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
                mtime: create.mtime,
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
            if let Some(hash) = update.hash {
                file.hash = hash;
            }
            if let Some(size) = update.size_bytes {
                file.size_bytes = size;
            }
            if let Some(mtime) = update.mtime {
                file.mtime = Some(mtime);
            }
            if let Some(mime_type) = update.mime_type {
                file.mime_type = Some(mime_type);
            }
            if let Some(duration) = update.duration {
                file.duration = Some(duration);
            }
            if let Some(container) = update.container_format {
                file.container_format = Some(container);
            }
            if let Some(status) = update.status {
                file.status = status;
            }
            if let Some(content) = update.content {
                file.content = Some(content);
            }
            file.updated_at = chrono::Utc::now();
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
