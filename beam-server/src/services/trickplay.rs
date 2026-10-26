//! Trickplay sheets, made in the background and served by Beam (issue #265).
//!
//! A worker walks every library's present, classified files and makes the
//! sheets of each one whose content has none yet -- or whose content changed
//! since its sheets were made -- one file at a time, so a library being
//! imported does not compete with playback for more than one core. A file
//! that gives no sheets (no video stream, frames that do not decode) is
//! recorded as such and not tried again until its content changes; one that
//! could not be read just now is left for the next pass, unrecorded
//! ([`beam_index::probe::trickplay::TrickplayError::is_content_fault`]).
//!
//! Everything is written under `{BEAM_DATA_DIR}/trickplay`, never beside the
//! library's files (FR-202; `AppServices::build` refuses a data directory
//! inside a library):
//!
//! ```text
//! trickplay/{file_id}/{content hash, 16 hex digits}/{sheet}.jpg
//! ```
//!
//! A file's sheets are made in a `.partial-*` folder beside the final one
//! and renamed into place whole, then recorded; only then are the sheets of
//! an older version of the file removed. A sweep after each pass removes the
//! folders of files whose record is gone -- the record goes with its file
//! when the file is purged -- and anything else no record names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::DbErr;
use tracing::{debug, info, warn};
use uuid::Uuid;

use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
use beam_domain::models::{Library, MediaFile};
use beam_domain::repositories::{FileRepository, LibraryRepository, TrickplayRepository};
use beam_domain::services::Clock;
use beam_index::library_file::LibraryFile;
use beam_index::probe::trickplay::TrickplaySpec;
use beam_index::services::trickplay::TrickplayGenerator;

/// How long the worker rests between passes. A file indexed just after a
/// pass has its sheets within this long, plus the time to make them.
pub const PASS_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The revision of how sheets are made, part of every sheet's validator: a
/// sheet is a function of the file *and* the generator, so a change to the
/// generator that changes the bytes must change this, or a client holding
/// an old sheet is told `304` and keeps it.
pub const GENERATOR_VERSION: u32 = 1;

/// What the service is built from.
#[derive(Debug, Clone)]
pub struct TrickplayDependencies {
    pub libraries: Arc<dyn LibraryRepository>,
    pub files: Arc<dyn FileRepository>,
    pub records: Arc<dyn TrickplayRepository>,
    pub generator: Arc<dyn TrickplayGenerator>,
    pub clock: Arc<dyn Clock>,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Files given sheets.
    pub made: u32,
    /// Files recorded as giving none.
    pub without_sheets: u32,
    /// Files left for the next pass: they could not be opened or read, or
    /// their sheets could not be written.
    pub deferred: u32,
}

/// One sheet of a file, located on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedSheet {
    /// The folder the sheet is opened beneath: the trickplay root.
    pub root: PathBuf,
    /// The sheet's path, beneath `root`.
    pub path: PathBuf,
    /// A strong validator: the file's content, the sheet, and the generator.
    pub etag: String,
}

/// Why a sheet could not be located.
#[derive(Debug, thiserror::Error)]
pub enum SheetLookupError {
    /// The file has no sheets for its current content, or not that many.
    #[error("no trickplay sheet")]
    NotFound,
    #[error("reading the trickplay record failed: {0}")]
    Db(#[from] DbErr),
}

/// Makes, records and locates trickplay sheets.
#[derive(Debug)]
pub struct TrickplayService {
    root: PathBuf,
    spec: TrickplaySpec,
    libraries: Arc<dyn LibraryRepository>,
    files: Arc<dyn FileRepository>,
    records: Arc<dyn TrickplayRepository>,
    generator: Arc<dyn TrickplayGenerator>,
    clock: Arc<dyn Clock>,
}

/// The folder name of the sheets made from content hashed `hash`.
fn hash_dir(hash: u64) -> String {
    format!("{hash:016x}")
}

/// Whether `file` is one sheets are made for: classified as a title's, with
/// a duration -- a file whose probe failed, or that is no title's, is not.
fn wants_sheets(file: &MediaFile) -> bool {
    file.content.is_some() && file.duration.is_some_and(|duration| !duration.is_zero())
}

/// The layout of `file`'s sheets as `record` describes them, when `record`
/// was made from the file's current content and gave any.
pub fn current_layout(
    file: &MediaFile,
    record: Option<&TrickplayRecord>,
) -> Option<TrickplayLayout> {
    record
        .filter(|record| record.file_hash == file.hash)
        .and_then(|record| record.layout)
}

impl TrickplayService {
    /// A service keeping its sheets under `root` (`{BEAM_DATA_DIR}/trickplay`),
    /// made as [`TrickplaySpec::default`] says.
    pub fn new(root: PathBuf, dependencies: TrickplayDependencies) -> Self {
        let TrickplayDependencies {
            libraries,
            files,
            records,
            generator,
            clock,
        } = dependencies;
        Self {
            root,
            spec: TrickplaySpec::default(),
            libraries,
            files,
            records,
            generator,
            clock,
        }
    }

    /// Make sheets as `spec` says rather than the default.
    pub fn with_spec(mut self, spec: TrickplaySpec) -> Self {
        self.spec = spec;
        self
    }

    /// The trickplay root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the sheet `sheet` of `file`'s current content is, if it has one.
    pub async fn locate_sheet(
        &self,
        file: &MediaFile,
        sheet: u32,
    ) -> Result<LocatedSheet, SheetLookupError> {
        let record = self.records.find(file.id).await?;
        let layout = current_layout(file, record.as_ref()).ok_or(SheetLookupError::NotFound)?;
        if sheet >= layout.sheet_count() {
            return Err(SheetLookupError::NotFound);
        }
        Ok(LocatedSheet {
            root: self.root.clone(),
            path: self
                .sheets_dir(file.id, file.hash)
                .join(format!("{sheet}.jpg")),
            etag: format!("{}-{sheet}-g{GENERATOR_VERSION}", hash_dir(file.hash)),
        })
    }

    /// Run passes forever, [`PASS_INTERVAL`] apart. A pass that fails to
    /// read the catalogue is logged and the next one tries again.
    pub async fn run(&self) {
        loop {
            match self.run_pass().await {
                Ok(report) if report == PassReport::default() => {
                    debug!("no file needed trickplay sheets");
                }
                Ok(report) => info!(?report, "trickplay pass finished"),
                Err(error) => warn!(%error, "trickplay pass failed"),
            }
            self.clock.sleep(PASS_INTERVAL).await;
        }
    }

    /// One pass: make the sheets of every file that needs them, then sweep
    /// away what no record names.
    pub async fn run_pass(&self) -> Result<PassReport, DbErr> {
        let mut report = PassReport::default();
        for library in self.libraries.find_all().await? {
            let files: Vec<MediaFile> = self
                .files
                .find_all_by_library(library.id)
                .await?
                .into_iter()
                .filter(wants_sheets)
                .collect();
            let ids: Vec<Uuid> = files.iter().map(|file| file.id).collect();
            let records: HashMap<Uuid, TrickplayRecord> = self
                .records
                .find_by_file_ids(&ids)
                .await?
                .into_iter()
                .map(|record| (record.file_id, record))
                .collect();
            for file in files {
                if self.is_current(&file, records.get(&file.id)).await {
                    continue;
                }
                match self.make(&library, &file).await? {
                    Made::Sheets => report.made += 1,
                    Made::None => report.without_sheets += 1,
                    Made::Deferred => report.deferred += 1,
                }
            }
        }
        self.sweep().await?;
        Ok(report)
    }

    /// Whether `record` is of `file`'s current content, and its sheets --
    /// if it gave any -- are still on disk: a data directory cleared by hand
    /// is made again rather than left recorded and missing.
    async fn is_current(&self, file: &MediaFile, record: Option<&TrickplayRecord>) -> bool {
        match record {
            Some(record) if record.file_hash == file.hash => match record.layout {
                None => true,
                Some(_) => tokio::fs::try_exists(self.sheets_dir(file.id, file.hash))
                    .await
                    .unwrap_or(false),
            },
            _ => false,
        }
    }

    /// The folder the sheets of `file_id`'s content hashed `hash` live in.
    fn sheets_dir(&self, file_id: Uuid, hash: u64) -> PathBuf {
        self.root.join(file_id.to_string()).join(hash_dir(hash))
    }

    /// Make and record `file`'s sheets.
    async fn make(&self, library: &Library, file: &MediaFile) -> Result<Made, DbErr> {
        let opened = match LibraryFile::open(&library.root_path, &file.path) {
            Ok(opened) => opened,
            Err(error) => {
                debug!(path = %file.path.display(), %error, "file not opened for trickplay");
                return Ok(Made::Deferred);
            }
        };
        let file_dir = self.root.join(file.id.to_string());
        let partial = file_dir.join(format!(".partial-{}", hash_dir(file.hash)));
        let finished = self.sheets_dir(file.id, file.hash);
        if let Err(error) = fresh_dir(&partial).await {
            warn!(dir = %partial.display(), %error, "could not prepare a trickplay folder");
            return Ok(Made::Deferred);
        }

        let layout = match self
            .generator
            .generate(opened, partial.clone(), self.spec)
            .await
        {
            Ok(Some(layout)) => match publish(&partial, &finished).await {
                Ok(()) => Some(layout),
                Err(error) => {
                    warn!(dir = %finished.display(), %error, "could not publish trickplay sheets");
                    let _ = tokio::fs::remove_dir_all(&partial).await;
                    return Ok(Made::Deferred);
                }
            },
            Ok(None) => None,
            // The content itself gives no sheets: recorded, and not tried
            // again until it changes.
            Err(error) if error.is_content_fault() => {
                warn!(path = %file.path.display(), %error, "no trickplay sheets for this file");
                None
            }
            // Anything else -- the file could not be rewound or read just
            // now, Beam's own folder could not be written, the work was
            // cancelled -- says nothing about the file, so it is tried again
            // rather than recorded: a flaky mount must not cost a file its
            // sheets until its content changes.
            Err(error) => {
                warn!(path = %file.path.display(), %error, "trickplay sheets left for a later pass");
                let _ = tokio::fs::remove_dir_all(&partial).await;
                return Ok(Made::Deferred);
            }
        };
        if layout.is_none() {
            let _ = tokio::fs::remove_dir_all(&partial).await;
        }

        self.records
            .upsert(TrickplayRecord {
                file_id: file.id,
                file_hash: file.hash,
                layout,
                generated_at: self.clock.now(),
            })
            .await?;
        // Only now that the record names the new sheets do the old go.
        remove_all_but(&file_dir, layout.map(|_| hash_dir(file.hash))).await;
        Ok(if layout.is_some() {
            Made::Sheets
        } else {
            Made::None
        })
    }

    /// Remove every folder under the root that no record names: a file's
    /// whole folder once its record is gone, and within a file's folder
    /// every version but the one its record names.
    async fn sweep(&self) -> Result<(), DbErr> {
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                warn!(root = %self.root.display(), %error, "could not list trickplay sheets");
                return Ok(());
            }
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Some(file_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
            else {
                continue;
            };
            match self.records.find(file_id).await? {
                None => {
                    if let Err(error) = tokio::fs::remove_dir_all(&path).await {
                        warn!(dir = %path.display(), %error, "could not remove trickplay sheets");
                    }
                }
                Some(record) => {
                    remove_all_but(&path, record.layout.map(|_| hash_dir(record.file_hash))).await;
                }
            }
        }
        Ok(())
    }
}

/// What making one file's sheets came to.
enum Made {
    Sheets,
    None,
    Deferred,
}

/// An empty folder at `dir`, whatever was there.
async fn fresh_dir(dir: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    tokio::fs::create_dir_all(dir).await
}

/// Move the finished sheets in `partial` to `finished`, replacing any there.
async fn publish(partial: &Path, finished: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_dir_all(finished).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    tokio::fs::rename(partial, finished).await
}

/// Remove everything in `dir` but the entry named `keep`; the folder itself
/// too when nothing is kept. Failures are logged: the next sweep retries.
async fn remove_all_but(dir: &Path, keep: Option<String>) {
    let Some(keep) = keep else {
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(dir = %dir.display(), %error, "could not remove trickplay sheets");
        }
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name().to_str() == Some(keep.as_str()) {
            continue;
        }
        let path = entry.path();
        let removed = match entry.file_type().await {
            Ok(kind) if kind.is_dir() => tokio::fs::remove_dir_all(&path).await,
            _ => tokio::fs::remove_file(&path).await,
        };
        if let Err(error) = removed {
            warn!(path = %path.display(), %error, "could not remove stale trickplay sheets");
        }
    }
}

#[cfg(test)]
#[path = "trickplay_tests.rs"]
mod trickplay_tests;
