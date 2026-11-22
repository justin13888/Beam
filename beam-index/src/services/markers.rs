use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::library_file::LibraryFile;
use crate::probe::fingerprint::{AudioFingerprint, FingerprintSpec};
use crate::probe::markers::{FingerprintError, fingerprint_file};

/// Fingerprints a file's audio for skip-marker detection (issue #332): the
/// seam between the background worker that decides which episodes to match
/// against which and the FFmpeg code that decodes their audio.
#[async_trait::async_trait]
pub trait AudioFingerprinter: Send + Sync + std::fmt::Debug {
    /// Fingerprint the opening and the ending of `file`'s audio, as much of
    /// each as `spec` says, reading the open handle alone and writing
    /// nothing. `None` for a file with no audio track.
    async fn fingerprint(
        &self,
        file: LibraryFile,
        spec: FingerprintSpec,
    ) -> Result<Option<AudioFingerprint>, FingerprintError>;
}

/// The FFmpeg fingerprinter, on a blocking thread.
///
/// Dropping the future cancels the decoding between two packets, so a
/// server told to stop does not wait for an episode's audio to finish.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalAudioFingerprinter;

/// Sets its flag when dropped.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[async_trait::async_trait]
impl AudioFingerprinter for LocalAudioFingerprinter {
    async fn fingerprint(
        &self,
        file: LibraryFile,
        spec: FingerprintSpec,
    ) -> Result<Option<AudioFingerprint>, FingerprintError> {
        let cancel = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancel));
        let flag = Arc::clone(&cancel);
        tokio::task::spawn_blocking(move || fingerprint_file(file, &spec, &flag))
            .await
            .map_err(|err| FingerprintError::Read(std::io::Error::other(err)))?
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fake {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use parking_lot::Mutex;

    use super::*;

    /// What the fake answers for one file.
    #[derive(Debug, Clone)]
    pub enum FakeListen {
        /// The file's audio, fingerprinted.
        Heard(AudioFingerprint),
        /// The file has no audio track.
        NoAudio,
        /// The file's content cannot be decoded: a content fault.
        Undecodable,
        /// The file could not be read just now: not a content fault.
        Unreadable,
    }

    /// The fake fingerprinter: answers each file by its path, as configured,
    /// and records every path it was asked for. A path it was not told about
    /// is unreadable.
    #[derive(Debug, Default)]
    pub struct FakeAudioFingerprinter {
        answers: Mutex<HashMap<PathBuf, FakeListen>>,
        heard: Mutex<Vec<PathBuf>>,
    }

    impl FakeAudioFingerprinter {
        /// Answer `path` with `listen` from now on.
        pub fn answer(&self, path: impl Into<PathBuf>, listen: FakeListen) {
            self.answers.lock().insert(path.into(), listen);
        }

        /// Every path asked for, in order.
        pub fn asked(&self) -> Vec<PathBuf> {
            self.heard.lock().clone()
        }

        fn listen(&self, path: &Path) -> FakeListen {
            self.heard.lock().push(path.to_path_buf());
            self.answers
                .lock()
                .get(path)
                .cloned()
                .unwrap_or(FakeListen::Unreadable)
        }
    }

    #[async_trait::async_trait]
    impl AudioFingerprinter for FakeAudioFingerprinter {
        async fn fingerprint(
            &self,
            file: LibraryFile,
            _spec: FingerprintSpec,
        ) -> Result<Option<AudioFingerprint>, FingerprintError> {
            match self.listen(file.path()) {
                FakeListen::Heard(print) => Ok(Some(print)),
                FakeListen::NoAudio => Ok(None),
                FakeListen::Undecodable => {
                    Err(FingerprintError::Ffmpeg(ffmpeg_next::Error::InvalidData))
                }
                FakeListen::Unreadable => Err(FingerprintError::Read(std::io::Error::other(
                    "the fake could not read it",
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag the blocking decode reads between packets is clear while the
    /// guard lives and set once it is dropped, as it is with the future that
    /// holds it.
    #[test]
    fn dropping_the_guard_cancels_the_listening_it_guards() {
        let cancel = Arc::new(AtomicBool::new(false));
        let guard = CancelOnDrop(Arc::clone(&cancel));
        assert!(!cancel.load(Ordering::Relaxed));

        drop(guard);

        assert!(cancel.load(Ordering::Relaxed));
    }
}
