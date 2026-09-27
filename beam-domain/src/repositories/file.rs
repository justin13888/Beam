use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::file::{CreateMediaFile, FileClassification, MediaFile, UpdateMediaFile};

/// Persistence for indexed media files.
///
/// A file the indexer can no longer find is *soft-deleted* (issue #179): its
/// row keeps its id and gains a `missing_since` stamp. Reads therefore come in
/// two kinds, and every method below says which it is.
///
/// * **Visible reads** answer "what can a user browse, open or stream?" and
///   never return a missing row. Everything outside the indexer uses these.
/// * **Reconcile reads** answer "what has the indexer ever recorded here?" and
///   include missing rows, so a path that comes back is restored under its old
///   id instead of being indexed as a new file.
///
/// [`FileRepository::purge_missing`] is the only way a row is ever removed,
/// and only a row that is already missing can be removed by it.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait FileRepository: Send + Sync + std::fmt::Debug {
    /// Visible read: `None` for a missing file.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr>;
    /// Reconcile read: returns the row for `path` whether or not it is
    /// missing, so the indexer can restore it under the same id.
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr>;
    /// Visible read: every present file whose content hash matches, across
    /// every library. Used for duplicate detection.
    async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr>;
    /// Visible read: every present file in the library.
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    /// Reconcile read: every file in the library, missing ones included. The
    /// scan compares the walk against this.
    async fn find_all_by_library_including_missing(
        &self,
        library_id: Uuid,
    ) -> Result<Vec<MediaFile>, DbErr>;
    /// Visible read.
    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    /// Visible read.
    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr>;
    /// Every write below refuses a row with no content whose status is not
    /// `Unknown`: a file is `Known` (or `Changed`) only as a movie's or an
    /// episode's file. Postgres enforces it with the `files` CHECK
    /// constraint.
    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr>;
    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr>;
    /// Replace `id`'s classification -- content, status and classifier
    /// version -- as a whole, clearing the content when `classification`
    /// carries none. Everything else about the row (its id, hash, probe
    /// results, missing stamp) is left as it is. Used when a scan reclassifies
    /// a file under newer rules (issue #182).
    async fn set_classification(
        &self,
        id: Uuid,
        classification: FileClassification,
    ) -> Result<MediaFile, DbErr>;
    /// Stamp `missing_since = at` on every listed row that is not already
    /// missing, returning how many were newly stamped. A row already missing
    /// keeps its first stamp: the grace period runs from when the file was
    /// first found gone, not from the latest scan that noticed. An empty list
    /// touches nothing.
    async fn mark_missing(&self, ids: Vec<Uuid>, at: DateTime<Utc>) -> Result<u64, DbErr>;
    /// Clear `missing_since` on `id`: the file is back on disk. The row keeps
    /// its id, so everything keyed on it (playback progress) is still there.
    async fn restore(&self, id: Uuid) -> Result<(), DbErr>;
    /// Hard-delete every listed row that is missing, returning how many went.
    /// A listed row that is present is left alone, so a caller racing a
    /// restore cannot purge a file that came back. An empty list touches
    /// nothing. The `ON DELETE CASCADE` foreign keys take the file's streams
    /// and playback progress with it.
    async fn purge_missing(&self, ids: Vec<Uuid>) -> Result<u64, DbErr>;
    /// Visible read: total number of present files across every library, for
    /// the admin status endpoint (issue #85).
    async fn count_all(&self) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::{FileStatus, MediaFileContent};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryFileRepository {
        pub files: Mutex<HashMap<Uuid, MediaFile>>,
    }

    /// The `files` CHECK constraint the Postgres schema enforces: a row with
    /// no content is `Unknown`.
    fn check_status(file: &MediaFile) -> Result<(), DbErr> {
        if file.content.is_none() && file.status != FileStatus::Unknown {
            return Err(DbErr::Custom(format!(
                "files CHECK: file {} has no content but status {:?}",
                file.id, file.status
            )));
        }
        Ok(())
    }

    #[async_trait]
    impl FileRepository for InMemoryFileRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .get(&id)
                .filter(|f| f.missing_since.is_none())
                .cloned())
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
                .filter(|f| f.missing_since.is_none() && f.hash == hash)
                .cloned()
                .collect())
        }

        async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| f.missing_since.is_none() && f.library_id == library_id)
                .cloned()
                .collect())
        }

        async fn find_all_by_library_including_missing(
            &self,
            library_id: Uuid,
        ) -> Result<Vec<MediaFile>, DbErr> {
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
                    f.missing_since.is_none()
                        && matches!(&f.content, Some(MediaFileContent::Movie { movie_entry_id: id }) if *id == movie_entry_id)
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
                    f.missing_since.is_none()
                        && matches!(&f.content, Some(MediaFileContent::Episode { episode_id: id, .. }) if *id == episode_id)
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
                missing_since: None,
                classifier_version: create.classifier_version,
            };
            check_status(&file)?;
            self.files.lock().unwrap().insert(file.id, file.clone());
            Ok(file)
        }

        async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
            let mut files = self.files.lock().unwrap();
            let stored = files
                .get_mut(&update.id)
                .ok_or(DbErr::RecordNotFound(format!(
                    "File {} not found",
                    update.id
                )))?;
            // Applied to a copy, so a write the CHECK refuses changes nothing.
            let mut updated = stored.clone();
            let file = &mut updated;
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
            check_status(&updated)?;
            *stored = updated.clone();
            Ok(updated)
        }

        async fn set_classification(
            &self,
            id: Uuid,
            classification: FileClassification,
        ) -> Result<MediaFile, DbErr> {
            let FileClassification {
                content,
                status,
                classifier_version,
            } = classification;
            let mut files = self.files.lock().unwrap();
            let stored = files
                .get_mut(&id)
                .ok_or(DbErr::RecordNotFound(format!("File {id} not found")))?;
            let mut file = stored.clone();
            file.content = content;
            file.status = status;
            file.classifier_version = classifier_version;
            file.updated_at = chrono::Utc::now();
            check_status(&file)?;
            *stored = file.clone();
            Ok(file)
        }

        async fn mark_missing(&self, ids: Vec<Uuid>, at: DateTime<Utc>) -> Result<u64, DbErr> {
            let mut files = self.files.lock().unwrap();
            let mut count = 0u64;
            for id in ids {
                if let Some(file) = files.get_mut(&id)
                    && file.missing_since.is_none()
                {
                    file.missing_since = Some(at);
                    count += 1;
                }
            }
            Ok(count)
        }

        async fn restore(&self, id: Uuid) -> Result<(), DbErr> {
            if let Some(file) = self.files.lock().unwrap().get_mut(&id) {
                file.missing_since = None;
            }
            Ok(())
        }

        async fn purge_missing(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
            let mut files = self.files.lock().unwrap();
            let mut count = 0u64;
            for id in ids {
                if files.get(&id).is_some_and(|f| f.missing_since.is_some()) {
                    files.remove(&id);
                    count += 1;
                }
            }
            Ok(count)
        }

        async fn count_all(&self) -> Result<u64, DbErr> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| f.missing_since.is_none())
                .count() as u64)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::FileRepository;
    use super::in_memory::InMemoryFileRepository;
    use crate::repositories::contract::fixture::FileRepositoryFixture;

    /// The hermetic instantiation of the shared contract. The in-memory store
    /// enforces no referential integrity, so a fresh v4 UUID is a valid parent;
    /// the Postgres fixture in `beam-index` inserts real rows for the same
    /// calls.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryFileRepository,
    }

    #[async_trait::async_trait]
    impl FileRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn FileRepository {
            &self.repo
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_movie_entry(&self, _library_id: Uuid) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_episode(&self, _library_id: Uuid) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::file_repository_contract!(setup);
}
