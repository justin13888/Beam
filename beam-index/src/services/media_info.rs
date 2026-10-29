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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Waker};

    use super::*;

    /// A file beneath a fresh library root that is no media: probing it fails
    /// at once, which is all these tests need of a probe.
    fn noise() -> (tempfile::TempDir, LibraryFile) {
        let _ = crate::probe::init();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("noise.mkv");
        std::fs::write(&path, [0x5a_u8; 4096]).unwrap();
        let file = LibraryFile::open(dir.path(), &path).unwrap();
        (dir, file)
    }

    /// A limited service starts no probe while every permit is taken: the
    /// probe waits in the semaphore's queue, is handed the permit the moment
    /// it is given back, and gives it back itself when done -- even when the
    /// probe fails, so a failing file can never use up the limit.
    #[tokio::test]
    async fn a_probe_waits_for_a_permit_and_gives_it_back_when_done() {
        let service = LocalMediaInfoService::new(1);
        let semaphore = Arc::clone(service.semaphore.as_ref().expect("a limited service"));
        let (_dir, file) = noise();
        let held = Arc::clone(&semaphore).acquire_owned().await.unwrap();

        let mut probe = pin!(service.get_video_metadata(file));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            probe.as_mut().poll(&mut cx).is_pending(),
            "no permit to run on"
        );

        drop(held);
        assert_eq!(
            semaphore.available_permits(),
            0,
            "the permit given back goes to the waiting probe"
        );
        let probed = probe.await;

        assert!(probed.is_err(), "noise is no media");
        assert_eq!(
            semaphore.available_permits(),
            1,
            "the probe gave its permit back"
        );
    }
}
