use std::path::PathBuf;

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
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalTrickplayGenerator;

#[async_trait::async_trait]
impl TrickplayGenerator for LocalTrickplayGenerator {
    async fn generate(
        &self,
        file: LibraryFile,
        out_dir: PathBuf,
        spec: TrickplaySpec,
    ) -> Result<Option<TrickplayLayout>, TrickplayError> {
        tokio::task::spawn_blocking(move || generate_sheets(file, &out_dir, &spec))
            .await
            .map_err(|err| TrickplayError::Write(std::io::Error::other(err)))?
    }
}
