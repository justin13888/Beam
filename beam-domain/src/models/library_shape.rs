//! The aggregate shape of every library on a server -- counts and
//! distributions, never an individual title, path or file (issue #93).

use crate::utils::telemetry::FileSizeHistogram;

/// How many things of one name there are, e.g. files in the `matroska`
/// container or audio streams in `aac`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedCount {
    pub name: String,
    pub count: u64,
}

impl NamedCount {
    pub fn new(name: impl Into<String>, count: u64) -> Self {
        Self {
            name: name.into(),
            count,
        }
    }
}

/// Present files, split by what the indexer classified them as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilesByContentType {
    pub movie: u64,
    pub episode: u64,
    /// Files the indexer could not classify as either.
    pub unclassified: u64,
}

impl FilesByContentType {
    pub fn total(&self) -> u64 {
        self.movie + self.episode + self.unclassified
    }
}

/// What [`crate::repositories::LibraryShapeRepository::shape`] answers.
///
/// Only what a user could browse counts: a soft-deleted file (issue #179) is
/// not in any count, its streams are not in any distribution, and a title
/// counts only while it is live -- while at least one of its files is present.
///
/// Every list is sorted by name, so two reads of one store compare equal. A
/// container the prober could not name is counted under
/// [`crate::utils::telemetry::UNKNOWN_LABEL`]; names are otherwise raw, and
/// normalising them for a report is the report's business.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryShape {
    /// Every registered library, empty or not.
    pub libraries: u64,
    /// Live movies.
    pub movies: u64,
    /// Live shows.
    pub shows: u64,
    /// Seasons with at least one episode that has a present file.
    pub seasons: u64,
    /// Episodes with a present file.
    pub episodes: u64,
    pub files: FilesByContentType,
    /// Present files per container.
    pub containers: Vec<NamedCount>,
    /// Streams of present files, per codec.
    pub video_codecs: Vec<NamedCount>,
    pub audio_codecs: Vec<NamedCount>,
    pub subtitle_codecs: Vec<NamedCount>,
    /// Present files per size bucket.
    pub file_sizes: FileSizeHistogram,
    /// Bytes across every present file.
    pub total_bytes: u64,
}
