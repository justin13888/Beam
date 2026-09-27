//! Moved and renamed files keep their rows (issue #180).
//!
//! A file that vanished from one path and a new file with the same content at
//! another, in the same library, are one file: its row is pointed at the new
//! path, keeping its id -- and so its playback progress, its movie or
//! episode, and its streams -- and the file is neither probed nor classified
//! again. Directory events reconcile the directory's subtree.
//!
//! The filesystem is a real `TempDir` and the hasher the real one, so a
//! "same file" is a file with the same bytes. The prober is a counting
//! double: no relink may probe.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::probe::metadata::MetadataError;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::hash::{HashConfig, LocalHashService};
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::CreateLibrary;
use beam_domain::models::admin_log::AdminLog;
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

// ─── choose_relink_candidate ────────────────────────────────────────────────

fn instant(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid instant")
}

fn candidate(id: u128, path: &str, missing_since: Option<DateTime<Utc>>) -> MediaFile {
    MediaFile {
        id: Uuid::from_u128(id),
        library_id: Uuid::nil(),
        path: PathBuf::from(path),
        hash: 77,
        size_bytes: 1000,
        mtime: None,
        mime_type: None,
        duration: None,
        container_format: None,
        content: None,
        status: FileStatus::Unknown,
        classifier_version: 0,
        scanned_at: instant(0),
        updated_at: instant(0),
        missing_since,
    }
}

/// Whether each kind of row can be the moved file's at all.
#[test]
fn only_a_gone_row_with_the_same_content_is_a_candidate() {
    let new_path = Path::new("/lib/new/Film.mkv");
    let gone = Some(instant(0));
    let absent = |path: &Path| path.starts_with("/lib/absent");

    struct Case {
        name: &'static str,
        row: MediaFile,
        hash: u64,
        matches: bool,
    }
    let with = |mut row: MediaFile, change: fn(&mut MediaFile)| {
        change(&mut row);
        row
    };
    let cases = [
        Case {
            name: "missing, same hash and size",
            row: candidate(1, "/lib/old/Film.mkv", gone),
            hash: 77,
            matches: true,
        },
        Case {
            name: "not yet marked, but gone from disk",
            row: candidate(1, "/lib/absent/Film.mkv", None),
            hash: 77,
            matches: true,
        },
        Case {
            name: "still on disk: a copy, not a move",
            row: candidate(1, "/lib/old/Film.mkv", None),
            hash: 77,
            matches: false,
        },
        Case {
            name: "another hash",
            row: candidate(1, "/lib/old/Film.mkv", gone),
            hash: 78,
            matches: false,
        },
        Case {
            name: "same hash, another size",
            row: with(candidate(1, "/lib/old/Film.mkv", gone), |row| {
                row.size_bytes = 999
            }),
            hash: 77,
            matches: false,
        },
        Case {
            name: "the unhashed sentinel matches nothing",
            row: with(candidate(1, "/lib/old/Film.mkv", gone), |row| row.hash = 0),
            hash: 0,
            matches: false,
        },
        Case {
            name: "the row already at the new path",
            row: candidate(1, "/lib/new/Film.mkv", gone),
            hash: 77,
            matches: false,
        },
    ];
    for Case {
        name,
        row,
        hash,
        matches,
    } in cases
    {
        let rows = [row];
        let chosen = choose_relink_candidate(new_path, 1000, hash, &rows, absent);
        assert_eq!(chosen.is_some(), matches, "{name}");
    }
}

/// Among several candidates the one most like the moved file wins, and the
/// choice does not depend on the order the rows came in.
#[test]
fn the_most_alike_candidate_wins_whatever_the_order() {
    let new_path = Path::new("/lib/b/One.mkv");
    let never_absent = |_: &Path| false;
    let cases: [(&str, Vec<MediaFile>, u128); 4] = [
        (
            "the same file name beats the same directory",
            vec![
                candidate(1, "/lib/b/Two.mkv", Some(instant(9))),
                candidate(2, "/lib/a/One.mkv", Some(instant(1))),
            ],
            2,
        ),
        (
            "then the same directory",
            vec![
                candidate(1, "/lib/a/Two.mkv", Some(instant(9))),
                candidate(2, "/lib/b/Two.mkv", Some(instant(1))),
            ],
            2,
        ),
        (
            "then the one gone most recently",
            vec![
                candidate(1, "/lib/a/Two.mkv", Some(instant(1))),
                candidate(2, "/lib/a/Six.mkv", Some(instant(9))),
            ],
            2,
        ),
        (
            "then the lowest id",
            vec![
                candidate(2, "/lib/a/Two.mkv", Some(instant(5))),
                candidate(1, "/lib/c/Two.mkv", Some(instant(5))),
            ],
            1,
        ),
    ];
    for (name, rows, expected) in cases {
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            let chosen = choose_relink_candidate(new_path, 1000, 77, &rows, never_absent);
            assert_eq!(
                chosen.map(|row| row.id),
                Some(Uuid::from_u128(expected)),
                "{name}"
            );
        }
    }
}

/// A row gone from disk but not marked yet was gone most recently of all:
/// the watcher has not reconciled its path yet.
#[test]
fn an_unmarked_absent_row_counts_as_gone_most_recently() {
    let rows = [
        candidate(1, "/lib/a/Two.mkv", Some(instant(9))),
        candidate(2, "/lib/absent/Six.mkv", None),
    ];
    let chosen = choose_relink_candidate(Path::new("/lib/b/One.mkv"), 1000, 77, &rows, |path| {
        path.starts_with("/lib/absent")
    });
    assert_eq!(chosen.map(|row| row.id), Some(Uuid::from_u128(2)));
}

// ─── the scan and the watcher ────────────────────────────────────────────────

/// A prober that reports an hour-long matroska file, or fails, counting its
/// calls: a relinked file is never probed.
#[derive(Debug, Default)]
struct CountingProber {
    calls: AtomicUsize,
    fails: AtomicBool,
}

#[async_trait::async_trait]
impl MediaInfoService for CountingProber {
    async fn get_video_metadata(&self, path: &Path) -> Result<VideoFileMetadata, MetadataError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fails.load(Ordering::SeqCst) {
            return Err(MetadataError::UnknownError(
                "moov atom not found".to_string(),
            ));
        }
        Ok(VideoFileMetadata {
            file_path: path.to_path_buf(),
            metadata: HashMap::default(),
            best_video_stream: None,
            best_audio_stream: None,
            best_subtitle_stream: None,
            duration: Duration::from_secs(60 * 60).as_micros() as i64,
            streams: vec![],
            format_name: "matroska".to_string(),
            format_long_name: "Matroska".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
        })
    }
}

/// The real hasher, counting its calls.
#[derive(Debug)]
struct CountingHasher {
    inner: LocalHashService,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl HashService for CountingHasher {
    fn hash_sync(&self, path: &Path) -> std::io::Result<u64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.hash_sync(path)
    }

    async fn hash_async(&self, path: PathBuf) -> std::io::Result<u64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.hash_async(path).await
    }
}

struct Harness {
    dir: TempDir,
    root: PathBuf,
    library: Library,
    library_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    notifications: Arc<InMemoryNotificationService>,
    prober: Arc<CountingProber>,
    hasher: Arc<CountingHasher>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        Self::with_service(|service| service).await
    }

    /// A harness whose service `configure` finishes building.
    async fn with_service(configure: impl FnOnce(LocalIndexService) -> LocalIndexService) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let notifications = Arc::new(InMemoryNotificationService::new());
        let prober = Arc::new(CountingProber::default());
        let hasher = Arc::new(CountingHasher {
            inner: LocalHashService::new(HashConfig { num_threads: 1 }),
            calls: AtomicUsize::new(0),
        });
        let library = library_repo
            .create(CreateLibrary {
                name: "Moves".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let service = LocalIndexService::new(
            library_repo.clone(),
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryMediaStreamRepository::default()),
            hasher.clone(),
            prober.clone(),
            notifications.clone(),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
        )
        .with_clock(Arc::new(TestClock::starting_at(instant(0))));
        let service = configure(service);
        Self {
            dir,
            root,
            library,
            library_repo,
            file_repo,
            admin_log_repo,
            notifications,
            prober,
            hasher,
            service,
        }
    }

    /// Write `rel` under the root with `content` as its bytes: two files with
    /// the same content are the same file to the indexer.
    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Move `from` to `rel` under the root.
    fn mv(&self, from: &Path, rel: &str) -> PathBuf {
        let to = self.root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(from, &to).unwrap();
        to
    }

    async fn scan(&self) -> ScanProgress {
        self.scan_library(self.library.id).await
    }

    async fn scan_library(&self, library_id: Uuid) -> ScanProgress {
        self.service
            .scan_now(library_id, ScanTrigger::Manual)
            .await
            .expect("the scan runs")
    }

    async fn reconcile(&self, path: &Path, kind: FsEventKind) -> ReconcileOutcome {
        self.service
            .reconcile_path(self.library.id, path.to_path_buf(), kind)
            .await
            .expect("the event reconciles")
    }

    async fn row(&self, path: &Path) -> Option<MediaFile> {
        self.file_repo
            .find_by_path(&path.to_string_lossy())
            .await
            .unwrap()
    }

    async fn present(&self, path: &Path) -> MediaFile {
        let row = self
            .row(path)
            .await
            .unwrap_or_else(|| panic!("no row at {}", path.display()));
        assert_eq!(row.missing_since, None, "{} is missing", path.display());
        row
    }

    fn probes(&self) -> usize {
        self.prober.calls.load(Ordering::SeqCst)
    }

    fn hashes(&self) -> usize {
        self.hasher.calls.load(Ordering::SeqCst)
    }

    async fn row_count(&self) -> usize {
        self.file_repo.files.lock().unwrap().len()
    }

    async fn move_logs(&self) -> Vec<AdminLog> {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|log| log.message.starts_with("File moved"))
            .collect()
    }
}

/// A renamed file is the same row: same id, same title, no probe, and
/// nothing marked missing, since the row moved with the file.
#[tokio::test]
async fn a_renamed_file_keeps_its_row_through_a_scan() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&old).await;
    assert!(before.content.is_some(), "classified as a movie");
    let probes = h.probes();

    let new = h.mv(&old, "Heat.1995.1080p.mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.added, progress.marked_missing),
        (1, 0, 0)
    );
    let after = h.present(&new).await;
    assert_eq!(after.id, before.id);
    assert_eq!(after.content, before.content, "the title is kept");
    assert_eq!(
        (after.duration, after.status, after.classifier_version),
        (before.duration, before.status, before.classifier_version)
    );
    assert!(
        h.row(&old).await.is_none(),
        "nothing is left at the old path"
    );
    assert_eq!(h.probes(), probes, "a relinked file is not probed");
    assert_eq!(h.row_count().await, 1);

    let logs = h.move_logs().await;
    assert_eq!(logs.len(), 1, "the move is logged once");
    let details = logs[0].details.as_ref().unwrap();
    assert_eq!(details["file_id"], serde_json::json!(before.id.to_string()));
    assert_eq!(
        details["from"],
        serde_json::json!(old.display().to_string())
    );
    assert_eq!(details["to"], serde_json::json!(new.display().to_string()));
    let completion = h
        .admin_log_repo
        .list(100, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|log| log.message.contains("scan completed"))
        .and_then(|log| log.details)
        .unwrap();
    assert_eq!(completion["relinked"], serde_json::json!(1));
}

#[tokio::test]
async fn a_file_moved_to_another_directory_keeps_its_row() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995)/Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&old).await;

    let new = h.mv(&old, "Crime/Heat (1995).mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&new).await.id, before.id);
    assert!(h.row(&old).await.is_none());
}

/// Moved out of the library a file is missing, not deleted; moved back --
/// to its old path or to a new one -- it is the same row again.
#[tokio::test]
async fn a_file_moved_out_and_back_in_is_the_same_row() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = h.present(&old).await;
    let outside = h.dir.path().join("outside/Heat (1995).mkv");

    std::fs::rename(&old, &outside).unwrap();
    let progress = h.scan().await;
    assert_eq!(progress.marked_missing, 1);
    let missing = h.row(&old).await.expect("marked missing, not deleted");
    assert!(missing.missing_since.is_some());

    let back = h.mv(&outside, "Films/Heat (1995).mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&back).await.id, before.id, "visible again");
}

/// A copy leaves the original where it is: the copy is a new file, and a
/// duplicate of it.
#[tokio::test]
async fn a_copied_file_is_a_new_row_and_a_duplicate() {
    let h = Harness::new().await;
    let original = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&original).await;

    let copy = h.write("Backup/Heat (1995).mkv", "heat");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (0, 1));
    assert_eq!(h.present(&original).await.id, before.id);
    assert_ne!(h.present(&copy).await.id, before.id);
    assert!(
        h.notifications
            .published_events()
            .iter()
            .any(|event| event.message.starts_with("Duplicate content")),
        "the copy is reported as a duplicate"
    );
}

/// A file moved into another library is that library's new file; the row
/// it left behind is not relinked across libraries.
#[tokio::test]
async fn a_file_moved_to_another_library_is_not_relinked_across() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = h.present(&old).await;
    let other_root = h.dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = h
        .library_repo
        .create(CreateLibrary {
            name: "Other".to_string(),
            root_path: other_root.clone(),
            description: None,
        })
        .await
        .unwrap();

    let moved = other_root.join("Heat (1995).mkv");
    std::fs::rename(&old, &moved).unwrap();
    let progress = h.scan_library(other.id).await;

    assert_eq!((progress.relinked, progress.added), (0, 1));
    let theirs = h.present(&moved).await;
    assert_ne!(theirs.id, before.id);
    assert_eq!(theirs.library_id, other.id);
    assert_eq!(
        h.row(&old).await.map(|row| row.id),
        Some(before.id),
        "the old library's row is left for its own scan"
    );
}

/// Two copies of one file moved together each keep their own row, matched
/// by name.
#[tokio::test]
async fn identical_files_moved_together_keep_their_own_rows() {
    let h = Harness::new().await;
    let one = h.write("Copies/One.mkv", "same");
    let two = h.write("Copies/Two.mkv", "same");
    h.scan().await;
    let (one_id, two_id) = (h.present(&one).await.id, h.present(&two).await.id);

    std::fs::rename(h.root.join("Copies"), h.root.join("Moved")).unwrap();
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (2, 0));
    assert_eq!(h.present(&h.root.join("Moved/One.mkv")).await.id, one_id);
    assert_eq!(h.present(&h.root.join("Moved/Two.mkv")).await.id, two_id);
}

/// The watcher reports the two halves of a rename as two events, in no
/// particular order. Either way the new path keeps the row.
#[tokio::test]
async fn a_renamed_file_keeps_its_row_whichever_watcher_event_comes_first() {
    for old_first in [true, false] {
        let h = Harness::new().await;
        let old = h.write("Heat (1995).mkv", "heat");
        h.scan().await;
        let before = h.present(&old).await;
        let probes = h.probes();

        let new = h.mv(&old, "Heat (1995) 1080p.mkv");
        let (first, second) = if old_first {
            ((&old, FsEventKind::Removed), (&new, FsEventKind::Created))
        } else {
            ((&new, FsEventKind::Created), (&old, FsEventKind::Removed))
        };
        assert_eq!(h.reconcile(first.0, first.1).await, ReconcileOutcome::Done);
        assert_eq!(
            h.reconcile(second.0, second.1).await,
            ReconcileOutcome::Done
        );

        assert_eq!(
            h.present(&new).await.id,
            before.id,
            "old first: {old_first}"
        );
        assert!(h.row(&old).await.is_none());
        assert_eq!(h.probes(), probes);
        assert_eq!(h.row_count().await, 1);
    }
}

/// A watcher sees a copy as a new file beside the original.
#[tokio::test]
async fn a_watcher_event_for_a_copy_indexes_a_new_row() {
    let h = Harness::new().await;
    let original = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&original).await;

    let copy = h.write("Heat (1995) copy.mkv", "heat");
    h.reconcile(&copy, FsEventKind::Created).await;

    assert_ne!(h.present(&copy).await.id, before.id);
    assert_eq!(h.present(&original).await.id, before.id);
}

/// A renamed season folder arrives as an event for each directory name. In
/// either order every episode keeps its row, and nothing is left missing.
#[tokio::test]
async fn a_renamed_directory_keeps_its_files_rows_in_either_event_order() {
    for old_first in [true, false] {
        let h = Harness::new().await;
        let e1 = h.write("Show/S01/Show S01E01.mkv", "one");
        let e2 = h.write("Show/S01/Show S01E02.mkv", "two");
        h.scan().await;
        let (id1, id2) = (h.present(&e1).await.id, h.present(&e2).await.id);
        let probes = h.probes();

        let old_dir = h.root.join("Show/S01");
        let new_dir = h.root.join("Show/Season 1");
        std::fs::rename(&old_dir, &new_dir).unwrap();
        let events = if old_first {
            [
                (&old_dir, FsEventKind::Modified),
                (&new_dir, FsEventKind::Modified),
            ]
        } else {
            [
                (&new_dir, FsEventKind::Modified),
                (&old_dir, FsEventKind::Modified),
            ]
        };
        for (path, kind) in events {
            assert_eq!(h.reconcile(path, kind).await, ReconcileOutcome::Done);
        }

        let moved1 = h.present(&new_dir.join("Show S01E01.mkv")).await;
        let moved2 = h.present(&new_dir.join("Show S01E02.mkv")).await;
        assert_eq!((moved1.id, moved2.id), (id1, id2), "old first: {old_first}");
        assert_eq!(h.row_count().await, 2, "no row left behind");
        assert_eq!(h.probes(), probes, "nothing probed");
    }
}

/// A removed directory marks the rows beneath it missing -- by whole path
/// components, so `S1` is not `S10` -- and deletes none.
#[tokio::test]
async fn a_removed_directory_marks_only_its_own_files_missing() {
    let h = Harness::new().await;
    let inside = h.write("Show/S1/Show S01E01.mkv", "one");
    let sibling = h.write("Show/S10/Show S10E01.mkv", "ten");
    h.scan().await;

    std::fs::remove_dir_all(h.root.join("Show/S1")).unwrap();
    h.reconcile(&h.root.join("Show/S1"), FsEventKind::Removed)
        .await;

    let gone = h.row(&inside).await.expect("kept, not deleted");
    assert!(gone.missing_since.is_some());
    h.present(&sibling).await;
}

/// A directory moved in from outside the library is walked and its files
/// indexed.
#[tokio::test]
async fn a_directory_moved_into_the_library_is_indexed() {
    let h = Harness::new().await;
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let outside = h.dir.path().join("outside/Heat (1995)");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Heat (1995).mkv"), "heat").unwrap();

    let moved_in = h.root.join("Heat (1995)");
    std::fs::rename(&outside, &moved_in).unwrap();
    assert_eq!(
        h.reconcile(&moved_in, FsEventKind::Created).await,
        ReconcileOutcome::Done
    );

    let row = h.present(&moved_in.join("Heat (1995).mkv")).await;
    assert!(row.content.is_some(), "probed and classified as new");
}

/// A directory renamed into one the path policy excludes takes its files
/// out of the library: they are marked missing, not indexed at their new
/// paths.
#[tokio::test]
async fn a_directory_renamed_into_an_excluded_one_marks_its_files_missing() {
    let h = Harness::new().await;
    let film = h.write("Heat (1995)/Heat (1995).mkv", "heat");
    h.write("Heat (1995)/Clips/Opening.mkv", "opening");
    h.scan().await;
    let scene = h.root.join("Heat (1995)/Clips/Opening.mkv");
    assert!(h.row(&scene).await.is_some());

    let extras = h.root.join("Heat (1995)/Extras");
    std::fs::rename(h.root.join("Heat (1995)/Clips"), &extras).unwrap();
    h.reconcile(&extras, FsEventKind::Created).await;
    h.reconcile(&h.root.join("Heat (1995)/Clips"), FsEventKind::Removed)
        .await;

    assert!(h.row(&extras.join("Opening.mkv")).await.is_none());
    assert!(h.row(&scene).await.unwrap().missing_since.is_some());
    h.present(&film).await;
}

/// A file whose size or modification time moved but whose content did not,
/// and whose probe keeps failing, is hashed once: its new size and
/// modification time are recorded, so the next visit does not hash it again
/// to find the same content (PR #224 r2).
#[tokio::test]
async fn an_unprobed_file_touched_is_hashed_once_not_on_every_visit() {
    let h = Harness::new().await;
    h.prober.fails.store(true, Ordering::SeqCst);
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    assert_eq!(h.hashes(), 1);
    assert_eq!(h.present(&path).await.duration, None, "the probe failed");

    let touched = std::time::SystemTime::now() + Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(touched)
        .unwrap();
    h.scan().await;
    assert_eq!(h.hashes(), 2, "a moved mtime is hashed once");
    let (_, mtime) = read_fs_meta(&path).unwrap();
    assert_eq!(h.present(&path).await.mtime, mtime, "and recorded");

    h.scan().await;
    assert_eq!(h.hashes(), 2, "and not again");
    assert_eq!(h.probes(), 3, "while the probe is still retried");
}

/// An event for the library root itself is left to the scan: a root that
/// reads as empty may be a volume that is not mounted, and only the scan's
/// empty-root guard can tell. Nothing beneath it is marked missing.
#[tokio::test]
async fn a_directory_event_for_the_library_root_marks_nothing_missing() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let ronin = h.write("Films/Ronin (1998).mkv", "ronin");
    h.scan().await;

    // What an unmounted volume leaves behind: its empty mount point.
    std::fs::remove_file(&heat).unwrap();
    std::fs::remove_dir_all(h.root.join("Films")).unwrap();
    assert_eq!(
        h.reconcile(&h.root, FsEventKind::Modified).await,
        ReconcileOutcome::Done
    );

    h.present(&heat).await;
    h.present(&ronin).await;
}

/// A directory event covering a file still being written is deferred as a
/// whole, so the watcher comes back to the directory once the file has
/// settled; the files that had settled are reconciled meanwhile.
#[tokio::test]
async fn a_directory_event_with_a_file_still_being_written_is_deferred() {
    let now = Utc::now();
    let window = Duration::from_secs(60);
    let h = Harness::with_service(|service| {
        service
            .with_clock(Arc::new(TestClock::starting_at(now)))
            .with_settle_window(window)
    })
    .await;
    let settled = h.write("Show/S01/Show S01E01.mkv", "one");
    let copying = h.write("Show/S01/Show S01E02.mkv", "two");
    let set_mtime = |path: &Path, at: DateTime<Utc>| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::from(at))
            .unwrap();
    };
    set_mtime(&settled, now - chrono::TimeDelta::hours(1));
    set_mtime(&copying, now);

    let outcome = h
        .reconcile(&h.root.join("Show/S01"), FsEventKind::Created)
        .await;

    assert_eq!(
        outcome,
        ReconcileOutcome::Deferred {
            retry_after: window
        }
    );
    h.present(&settled).await;
    assert!(h.row(&copying).await.is_none(), "not indexed while written");
}

/// A watcher event's kind is a hint only. A file removed and written again
/// within one debounce window arrives as a `Removed` event for a path that
/// is there, and its row stays present.
#[tokio::test]
async fn a_removed_event_for_a_path_that_is_back_leaves_its_row_present() {
    let h = Harness::new().await;
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&path).await;

    std::fs::remove_file(&path).unwrap();
    h.write("Heat (1995).mkv", "heat");
    assert_eq!(
        h.reconcile(&path, FsEventKind::Removed).await,
        ReconcileOutcome::Done
    );

    assert_eq!(h.present(&path).await.id, before.id);
}

/// A removed directory is not believed while the library root holds no
/// video file at all -- what an unmounted volume looks like, as the scan's
/// empty-root guard reads it. Its rows are left for the next scan.
#[tokio::test]
async fn a_removed_directory_under_a_root_with_no_video_is_left_to_the_scan() {
    let h = Harness::new().await;
    let episode = h.write("Show/S1/Show S01E01.mkv", "one");
    let film = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;

    std::fs::remove_dir_all(h.root.join("Show")).unwrap();
    std::fs::remove_file(&film).unwrap();
    std::fs::write(h.root.join(".not_mounted"), "").unwrap();
    assert_eq!(
        h.reconcile(&h.root.join("Show/S1"), FsEventKind::Removed)
            .await,
        ReconcileOutcome::Done
    );

    h.present(&episode).await;
}

/// A removed path the policy never indexes anything beneath -- a hidden
/// partial download, a sidecar, an extras folder -- has no row of its own,
/// and the rows of the library are not read to look beneath it.
#[tokio::test]
async fn a_removed_path_the_policy_never_indexes_is_not_looked_beneath() {
    use crate::services::hash::MockHashService;
    use crate::services::media_info::MockMediaInfoService;
    use beam_domain::repositories::file::MockFileRepository;

    let dir = TempDir::new().unwrap();
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let library = library_repo
        .create(CreateLibrary {
            name: "Films".to_string(),
            root_path: dir.path().to_path_buf(),
            description: None,
        })
        .await
        .unwrap();
    let mut files = MockFileRepository::new();
    files.expect_find_by_path().returning(|_| Ok(None));
    files.expect_find_beneath_including_missing().never();
    let service = LocalIndexService::new(
        library_repo,
        Arc::new(files),
        Arc::new(InMemoryMovieRepository::default()),
        Arc::new(InMemoryShowRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        ))),
    );

    for removed in [
        ".Heat (1995).mkv.part",
        "Heat (1995).srt",
        "Heat (1995)/Extras",
    ] {
        assert_eq!(
            service
                .reconcile_path(library.id, dir.path().join(removed), FsEventKind::Removed)
                .await
                .unwrap(),
            ReconcileOutcome::Done,
            "{removed}"
        );
    }
}
