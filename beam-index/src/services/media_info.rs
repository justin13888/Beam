use std::num::NonZeroUsize;
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

/// Probes a file in-process, through the linked FFmpeg libraries, on a
/// blocking thread -- no FFmpeg process is spawned.
///
/// [`Default`] probes any number of files at once; [`Self::new`] bounds it.
#[derive(Debug, Clone, Default)]
pub struct LocalMediaInfoService {
    /// Limits how many probes run at once. [None] indicates no limit.
    semaphore: Option<Arc<Semaphore>>,
}

impl LocalMediaInfoService {
    /// A service that runs at most `concurrent_limit` probes at once; a
    /// further probe waits for one to finish. The limit is never zero, which
    /// would leave every probe waiting forever.
    pub fn new(concurrent_limit: NonZeroUsize) -> Self {
        Self {
            semaphore: Some(Arc::new(Semaphore::new(concurrent_limit.get()))),
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
        let service = LocalMediaInfoService::new(NonZeroUsize::MIN);
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
