use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beam_domain::models::trickplay::TrickplayLayout;

use crate::library_file::LibraryFile;
use crate::probe::trickplay::{TrickplayError, TrickplaySpec, generate_sheets};

/// Makes a file's trickplay sheets (issue #265): the seam between the
/// background worker that decides which files need them and the FFmpeg code
/// that decodes frames.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait TrickplayGenerator: Send + Sync + std::fmt::Debug {
    /// Make `file`'s sheets in `out_dir` -- `0.jpg`, `1.jpg` and so on --
    /// laid out as `spec` says, reading the open handle alone and writing
    /// nothing outside `out_dir`. `None` for a file with no video to make
    /// them from.
    async fn generate(
        &self,
        file: LibraryFile,
        out_dir: PathBuf,
        spec: TrickplaySpec,
    ) -> Result<Option<TrickplayLayout>, TrickplayError>;
}

/// The FFmpeg generator, on a blocking thread.
///
/// Dropping the future cancels the generation between two tiles. A runtime
/// shutting down drops its tasks' futures and then waits, without a bound,
/// for its blocking threads: without the cancel, a server told to stop would
/// sit until the current file's sheets were done, past its shutdown timeout.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalTrickplayGenerator;

/// Sets its flag when dropped.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[async_trait::async_trait]
impl TrickplayGenerator for LocalTrickplayGenerator {
    async fn generate(
        &self,
        file: LibraryFile,
        out_dir: PathBuf,
        spec: TrickplaySpec,
    ) -> Result<Option<TrickplayLayout>, TrickplayError> {
        let cancel = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancel));
        tokio::task::spawn_blocking(move || generate_sheets(file, &out_dir, &spec, &cancel))
            .await
            .map_err(|err| TrickplayError::Write(std::io::Error::other(err)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag the blocking generation reads before each tile is clear
    /// while the guard lives -- the generation runs -- and set once it is
    /// dropped, as it is with the future that holds it, whether that future
    /// finished or a shutting-down runtime dropped it part-way.
    #[test]
    fn dropping_the_guard_cancels_the_generation_it_guards() {
        let cancel = Arc::new(AtomicBool::new(false));
        let guard = CancelOnDrop(Arc::clone(&cancel));
        assert!(!cancel.load(Ordering::Relaxed), "running while guarded");

        drop(guard);

        assert!(cancel.load(Ordering::Relaxed), "cancelled once dropped");
    }
}
