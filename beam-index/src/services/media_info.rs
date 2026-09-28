use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::library_file::LibraryFile;
use crate::probe::metadata::{MetadataError, VideoFileMetadata};

/// Service for extracting media information from files.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait MediaInfoService: Send + Sync + std::fmt::Debug {
    /// Extract video metadata from a library file, reading the open handle
    /// alone -- never its path, which a link may have been swapped into
    /// since it was opened (issue #238).
    async fn get_video_metadata(
        &self,
        file: LibraryFile,
    ) -> Result<VideoFileMetadata, MetadataError>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalMediaInfoService {
    /// Limit the number of concurrent FFmpeg processes.
    /// [None] indicates no limit (unrestricted).
    semaphore: Option<Arc<Semaphore>>,
}

impl LocalMediaInfoService {
    /// Creates a new service with a specific concurrency limit.
    pub fn new(concurrent_limit: usize) -> Self {
        Self {
            semaphore: Some(Arc::new(Semaphore::new(concurrent_limit))),
        }
    }
}

#[async_trait::async_trait]
impl MediaInfoService for LocalMediaInfoService {
    async fn get_video_metadata(
        &self,
        file: LibraryFile,
    ) -> Result<VideoFileMetadata, MetadataError> {
        let _permit = if let Some(sem) = &self.semaphore {
            Some(sem.acquire().await.map_err(|e| {
                MetadataError::UnknownError(format!("Failed to acquire semaphore: {e}"))
            })?)
        } else {
            None
        };

        tokio::task::spawn_blocking(move || VideoFileMetadata::from_library_file(file))
            .await
            .map_err(|e| MetadataError::UnknownError(format!("Blocking task join error: {e}")))?
    }
}
