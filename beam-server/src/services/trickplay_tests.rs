//! The trickplay worker over a real `TempDir` library and data directory,
//! with in-memory repositories and a generator that writes stand-in sheets
//! (issue #265). The FFmpeg generator itself is tested against real media
//! in `beam-index`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use super::*;
use beam_domain::models::file::{CreateMediaFile, ProbeUpdate, UpdateMediaFile};
use beam_domain::models::{CreateLibrary, FileStatus, MediaFileContent};
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository;
use beam_domain::services::TestClock;
use beam_index::probe::metadata::MetadataError;
use beam_index::probe::trickplay::{TrickplayError, TrickplaySpec};

/// What the stand-in generator does with the next file.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// Write this many sheets and report a layout of that many full sheets.
    Sheets(u32),
    /// Report no video.
    NoVideo,
    /// Fail as a file that does not decode.
    Undecodable,
    /// Fail to write Beam's own folder.
    WriteFails,
    /// Fail to rewind the file before reading it.
    RewindFails,
    /// Fail to read the file part-way, as a flaky network mount does.
    ReadFails,
}

/// A generator that writes `{n}.jpg` stand-ins, counting its calls.
#[derive(Debug)]
struct StandInGenerator {
    outcome: Mutex<Outcome>,
    calls: AtomicUsize,
}

impl StandInGenerator {
    fn new(outcome: Outcome) -> Self {
        Self {
            outcome: Mutex::new(outcome),
            calls: AtomicUsize::new(0),
        }
    }

    fn set(&self, outcome: Outcome) {
        *self.outcome.lock().unwrap() = outcome;
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn layout(sheets: u32) -> TrickplayLayout {
    TrickplayLayout {
        interval_ms: 10_000,
        tile_width: 320,
        tile_height: 180,
        columns: 10,
        rows: 10,
        tile_count: sheets * 100,
    }
}

#[async_trait::async_trait]
impl TrickplayGenerator for StandInGenerator {
    async fn generate(
        &self,
        _file: LibraryFile,
        out_dir: PathBuf,
        _spec: TrickplaySpec,
    ) -> Result<Option<TrickplayLayout>, TrickplayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let outcome = *self.outcome.lock().unwrap();
        match outcome {
            Outcome::Sheets(sheets) => {
                for sheet in 0..sheets {
                    std::fs::write(out_dir.join(format!("{sheet}.jpg")), b"\xff\xd8 sheet")?;
                }
                Ok(Some(layout(sheets)))
            }
            Outcome::NoVideo => Ok(None),
            Outcome::Undecodable => Err(TrickplayError::Open(MetadataError::InvalidMetadata(
                "not a container".into(),
            ))),
            Outcome::WriteFails => Err(TrickplayError::Write(std::io::Error::other("disk full"))),
            Outcome::RewindFails => Err(TrickplayError::Open(MetadataError::UnknownError(
                "Failed to rewind".into(),
            ))),
            Outcome::ReadFails => Err(TrickplayError::Read(std::io::Error::from_raw_os_error(
                5, // EIO
            ))),
        }
    }
}

struct Harness {
    _dirs: (TempDir, TempDir),
    library_root: PathBuf,
    library: Library,
    files: Arc<InMemoryFileRepository>,
    records: Arc<InMemoryTrickplayRepository>,
    generator: Arc<StandInGenerator>,
    clock: Arc<TestClock>,
    service: TrickplayService,
}

impl Harness {
    async fn new(outcome: Outcome) -> Self {
        let library_dir = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        let library_root = library_dir.path().to_path_buf();
        let libraries = Arc::new(InMemoryLibraryRepository::default());
        let library = libraries
            .create(CreateLibrary {
                name: "Films".into(),
                root_path: library_root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let files = Arc::new(InMemoryFileRepository::default());
        let records = Arc::new(InMemoryTrickplayRepository::default());
        let generator = Arc::new(StandInGenerator::new(outcome));
        let clock = Arc::new(TestClock::new());
        let service = TrickplayService::new(
            data_dir.path().join("trickplay"),
            TrickplayDependencies {
                libraries,
                files: files.clone(),
                records: records.clone(),
                generator: generator.clone(),
                clock: clock.clone(),
            },
        );
        Self {
            _dirs: (library_dir, data_dir),
            library_root,
            library,
            files,
            records,
            generator,
            clock,
            service,
        }
    }

    /// A classified movie file at `name` in the library, on disk, hashed
    /// `hash`.
    async fn file(&self, name: &str, hash: u64) -> MediaFile {
        let path = self.library_root.join(name);
        std::fs::write(&path, b"not really a film").unwrap();
        self.files
            .create(CreateMediaFile {
                library_id: self.library.id,
                path,
                hash,
                size_bytes: 17,
                mtime: None,
                identity: None,
                mime_type: Some("video/x-matroska".into()),
                duration: Some(Duration::from_secs(7_200)),
                container_format: Some("matroska".into()),
                content: Some(MediaFileContent::movie(Uuid::new_v4())),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .unwrap()
    }

    /// The file's content changes: a new hash.
    async fn rehash(&self, file: &MediaFile, hash: u64) -> MediaFile {
        self.files
            .update(UpdateMediaFile {
                id: file.id,
                hash: Some(hash),
                size_bytes: None,
                mtime: None,
                identity: None,
                probe: ProbeUpdate::Keep,
                content: None,
                status: None,
            })
            .await
            .unwrap()
    }

    fn file_dir(&self, file: &MediaFile) -> PathBuf {
        self.service.root().join(file.id.to_string())
    }

    /// The names in `file`'s folder, sorted; empty when it has none.
    fn versions(&self, file: &MediaFile) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.file_dir(file)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[tokio::test]
async fn a_pass_makes_and_records_the_sheets_of_a_file_without_any_once() {
    let h = Harness::new(Outcome::Sheets(2)).await;
    let file = h.file("Film (2001).mkv", 0xabc).await;

    let report = h.service.run_pass().await.unwrap();

    assert_eq!(
        report,
        PassReport {
            made: 1,
            ..PassReport::default()
        }
    );
    assert_eq!(
        h.records.find(file.id).await.unwrap(),
        Some(TrickplayRecord {
            file_id: file.id,
            file_hash: 0xabc,
            layout: Some(layout(2)),
            generated_at: beam_domain::models::file::mtime_as_stored(h.clock.now()),
        })
    );
    assert_eq!(h.versions(&file), vec!["0000000000000abc"]);
    let sheets: Vec<String> = {
        let mut names: Vec<String> = std::fs::read_dir(h.file_dir(&file).join("0000000000000abc"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    assert_eq!(sheets, vec!["0.jpg", "1.jpg"]);

    // Nothing changed: the next pass makes nothing.
    assert_eq!(h.service.run_pass().await.unwrap(), PassReport::default());
    assert_eq!(h.generator.calls(), 1);
}

#[tokio::test]
async fn a_file_whose_content_changed_is_made_again_and_its_old_sheets_go() {
    let h = Harness::new(Outcome::Sheets(1)).await;
    let file = h.file("Film (2001).mkv", 1).await;
    h.service.run_pass().await.unwrap();

    let file = h.rehash(&file, 2).await;
    let report = h.service.run_pass().await.unwrap();

    assert_eq!(report.made, 1);
    assert_eq!(h.generator.calls(), 2);
    assert_eq!(h.versions(&file), vec!["0000000000000002"]);
    assert_eq!(
        h.records.find(file.id).await.unwrap().map(|r| r.file_hash),
        Some(2)
    );
}

#[tokio::test]
async fn sheets_deleted_from_the_data_directory_are_made_again() {
    let h = Harness::new(Outcome::Sheets(1)).await;
    let file = h.file("Film (2001).mkv", 8).await;
    h.service.run_pass().await.unwrap();

    std::fs::remove_dir_all(h.service.root()).unwrap();
    let report = h.service.run_pass().await.unwrap();

    assert_eq!(report.made, 1);
    assert_eq!(h.generator.calls(), 2);
    assert_eq!(h.versions(&file), vec!["0000000000000008"]);
}

#[tokio::test]
async fn a_file_that_gives_no_sheets_is_recorded_so_and_not_tried_again() {
    for outcome in [Outcome::NoVideo, Outcome::Undecodable] {
        let h = Harness::new(outcome).await;
        let file = h.file("Audio only.mkv", 5).await;

        let report = h.service.run_pass().await.unwrap();

        assert_eq!(report.without_sheets, 1, "{outcome:?}");
        let record = h.records.find(file.id).await.unwrap().expect("recorded");
        assert_eq!((record.file_hash, record.layout), (5, None), "{outcome:?}");
        assert!(h.versions(&file).is_empty(), "{outcome:?}: no folder left");

        h.service.run_pass().await.unwrap();
        assert_eq!(h.generator.calls(), 1, "{outcome:?}: not tried again");
    }
}

#[tokio::test]
async fn a_failure_that_says_nothing_about_the_file_is_retried_rather_than_recorded() {
    for outcome in [
        Outcome::WriteFails,
        Outcome::RewindFails,
        Outcome::ReadFails,
    ] {
        let h = Harness::new(outcome).await;
        let file = h.file("Film (2001).mkv", 9).await;

        let report = h.service.run_pass().await.unwrap();

        assert_eq!(report.deferred, 1, "{outcome:?}");
        assert_eq!(h.records.find(file.id).await.unwrap(), None, "{outcome:?}");
        assert!(h.versions(&file).is_empty(), "{outcome:?}");

        h.generator.set(Outcome::Sheets(1));
        assert_eq!(h.service.run_pass().await.unwrap().made, 1, "{outcome:?}");
    }
}

#[tokio::test]
async fn a_file_gone_from_disk_is_left_for_a_later_pass() {
    let h = Harness::new(Outcome::Sheets(1)).await;
    let file = h.file("Film (2001).mkv", 9).await;
    std::fs::remove_file(&file.path).unwrap();

    let report = h.service.run_pass().await.unwrap();

    assert_eq!(report.deferred, 1);
    assert_eq!(h.generator.calls(), 0);
    assert_eq!(h.records.find(file.id).await.unwrap(), None);
}

#[tokio::test]
async fn files_no_title_owns_or_with_no_duration_get_no_sheets() {
    let h = Harness::new(Outcome::Sheets(1)).await;
    let unclassified = h.file("Unknown.mkv", 1).await;
    h.files
        .set_classification(
            unclassified.id,
            beam_domain::models::file::FileClassification {
                content: None,
                status: FileStatus::Unknown,
                classifier_version: 0,
            },
        )
        .await
        .unwrap();
    let unprobed = h.file("Unprobed.mkv", 2).await;
    h.files
        .update(UpdateMediaFile {
            id: unprobed.id,
            hash: None,
            size_bytes: None,
            mtime: None,
            identity: None,
            probe: ProbeUpdate::Clear,
            content: None,
            status: None,
        })
        .await
        .unwrap();

    assert_eq!(h.service.run_pass().await.unwrap(), PassReport::default());
    assert_eq!(h.generator.calls(), 0);
}

#[tokio::test]
async fn the_sweep_removes_what_no_record_names() {
    let h = Harness::new(Outcome::Sheets(1)).await;
    let file = h.file("Film (2001).mkv", 3).await;
    h.service.run_pass().await.unwrap();
    // A crash mid-make left a partial folder and an old version beside the
    // current one; a purged file left a whole folder.
    std::fs::create_dir_all(h.file_dir(&file).join(".partial-0000000000000004")).unwrap();
    std::fs::create_dir_all(h.file_dir(&file).join("0000000000000001")).unwrap();
    let purged = h.service.root().join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(purged.join("0000000000000007")).unwrap();
    // Something that is not a file's folder is not Beam's to judge.
    let other = h.service.root().join("README");
    std::fs::write(&other, b"left alone").unwrap();

    h.service.run_pass().await.unwrap();

    assert_eq!(h.versions(&file), vec!["0000000000000003"]);
    assert!(!purged.exists());
    assert!(other.exists());
}

#[tokio::test]
async fn a_sheet_is_located_for_the_files_current_content_only() {
    let h = Harness::new(Outcome::Sheets(2)).await;
    let file = h.file("Film (2001).mkv", 0x10).await;
    h.service.run_pass().await.unwrap();

    let located = h.service.locate_sheet(&file, 1).await.unwrap();
    assert_eq!(located.root, h.service.root());
    assert_eq!(
        located.path,
        h.file_dir(&file).join("0000000000000010").join("1.jpg")
    );
    assert!(located.path.exists());
    assert_eq!(located.etag, "0000000000000010-1-g1");

    // Past the last sheet.
    assert!(matches!(
        h.service.locate_sheet(&file, 2).await,
        Err(SheetLookupError::NotFound)
    ));
    // The file changed and its sheets are not made yet.
    let changed = h.rehash(&file, 0x11).await;
    assert!(matches!(
        h.service.locate_sheet(&changed, 0).await,
        Err(SheetLookupError::NotFound)
    ));
}

#[tokio::test]
async fn a_file_with_no_sheets_locates_none() {
    let h = Harness::new(Outcome::NoVideo).await;
    let file = h.file("Audio only.mkv", 1).await;
    assert!(matches!(
        h.service.locate_sheet(&file, 0).await,
        Err(SheetLookupError::NotFound)
    ));
    h.service.run_pass().await.unwrap();
    assert!(matches!(
        h.service.locate_sheet(&file, 0).await,
        Err(SheetLookupError::NotFound)
    ));
}
