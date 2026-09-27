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
use beam_domain::models::playback_progress::UpsertPlaybackProgress;
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::PlaybackProgressRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
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
        let chosen = choose_relink_candidate(new_path, 1000, hash, &rows, absent, &HashMap::new());
        assert_eq!(chosen.is_some(), matches, "{name}");
    }
}

/// Among several candidates the one most like the moved file wins -- the
/// same name, then the same directory, then the one played most recently,
/// then the lowest id -- and the choice does not depend on the order the
/// rows came in.
#[test]
fn the_most_alike_candidate_wins_whatever_the_order() {
    let new_path = Path::new("/lib/b/One.mkv");
    let never_absent = |_: &Path| false;
    let gone = Some(instant(0));
    let played = |plays: &[(u128, i64)]| -> HashMap<Uuid, DateTime<Utc>> {
        plays
            .iter()
            .map(|(id, secs)| (Uuid::from_u128(*id), instant(*secs)))
            .collect()
    };
    type Plays = HashMap<Uuid, DateTime<Utc>>;
    let cases: [(&str, Vec<MediaFile>, Plays, u128); 5] = [
        (
            "the same file name beats the same directory, and being played",
            vec![
                candidate(1, "/lib/b/Two.mkv", gone),
                candidate(2, "/lib/a/One.mkv", gone),
            ],
            played(&[(1, 9)]),
            2,
        ),
        (
            "then the same directory beats being played",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/b/Two.mkv", gone),
            ],
            played(&[(1, 9)]),
            2,
        ),
        (
            "then the one played most recently",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/a/Six.mkv", gone),
            ],
            played(&[(1, 1), (2, 9)]),
            2,
        ),
        (
            "a row played at all beats one never played",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/a/Six.mkv", gone),
            ],
            played(&[(2, 1)]),
            2,
        ),
        (
            "then the lowest id",
            vec![
                candidate(2, "/lib/a/Two.mkv", gone),
                candidate(1, "/lib/c/Two.mkv", gone),
            ],
            played(&[(1, 5), (2, 5)]),
            1,
        ),
    ];
    for (name, rows, last_played, expected) in cases {
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            let chosen =
                choose_relink_candidate(new_path, 1000, 77, &rows, never_absent, &last_played);
            assert_eq!(
                chosen.map(|row| row.id),
                Some(Uuid::from_u128(expected)),
                "{name}"
            );
        }
    }
}

// ─── plan_content_moves ──────────────────────────────────────────────────────

/// A row of hash `hash` at `path`, 1000 bytes, with no modification time.
fn row(id: u128, path: &str, hash: u64) -> MediaFile {
    MediaFile {
        hash,
        ..candidate(id, path, None)
    }
}

/// `hash` found at a path, 1000 bytes.
fn found(hash: u64) -> Fingerprint {
    Fingerprint {
        size: 1000,
        mtime: None,
        hash,
    }
}

/// The moves a scan plans for `rows`, given what it found at the paths it
/// hashed (`fingerprints`) and the other paths it walked: each relink as
/// `(row id, path)`, and the displaced row ids.
fn moves(
    rows: &[MediaFile],
    walked: &[&str],
    fingerprints: &[(&str, u64)],
    shielded: &[&str],
    last_played: &HashMap<Uuid, DateTime<Utc>>,
) -> (Vec<(u128, PathBuf)>, Vec<u128>) {
    let rows: HashMap<PathBuf, MediaFile> = rows
        .iter()
        .map(|row| (row.path.clone(), row.clone()))
        .collect();
    let fingerprints: HashMap<PathBuf, Fingerprint> = fingerprints
        .iter()
        .map(|(path, hash)| (PathBuf::from(path), found(*hash)))
        .collect();
    let walked: std::collections::HashSet<&Path> = walked
        .iter()
        .map(Path::new)
        .chain(fingerprints.keys().map(PathBuf::as_path))
        .collect();
    let is_shielded = |path: &Path| shielded.iter().any(|failed| path.starts_with(failed));
    let matches = content_matches(&rows, &walked, &fingerprints, is_shielded);
    let ContentMoves { relinks, displaced } = plan_content_moves(&rows, matches, last_played);
    let mut relinks: Vec<(u128, PathBuf)> = relinks
        .into_iter()
        .map(|(row, path, _)| (row.id.as_u128(), path))
        .collect();
    relinks.sort();
    (
        relinks,
        displaced.into_iter().map(|row| row.id.as_u128()).collect(),
    )
}

fn to(id: u128, path: &str) -> (u128, PathBuf) {
    (id, PathBuf::from(path))
}

/// Each way content moves between the paths of one scan, and which rows
/// follow it. Every case is planned from the walk as the scan sees it.
#[test]
fn rows_follow_their_content_across_the_walk() {
    let none = HashMap::new();
    struct Case {
        name: &'static str,
        rows: Vec<MediaFile>,
        walked: Vec<&'static str>,
        fingerprints: Vec<(&'static str, u64)>,
        shielded: Vec<&'static str>,
        relinks: Vec<(u128, PathBuf)>,
        displaced: Vec<u128>,
    }
    let cases = [
        Case {
            name: "a rename",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv")],
            displaced: vec![],
        },
        Case {
            name: "a swap of two names",
            rows: vec![row(1, "/lib/a.mkv", 10), row(2, "/lib/b.mkv", 20)],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 20), ("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv"), to(2, "/lib/a.mkv")],
            displaced: vec![],
        },
        Case {
            name: "a rotation of three",
            rows: vec![
                row(1, "/lib/a.mkv", 10),
                row(2, "/lib/b.mkv", 20),
                row(3, "/lib/c.mkv", 30),
            ],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 30), ("/lib/b.mkv", 10), ("/lib/c.mkv", 20)],
            shielded: vec![],
            relinks: vec![
                to(1, "/lib/b.mkv"),
                to(2, "/lib/c.mkv"),
                to(3, "/lib/a.mkv"),
            ],
            displaced: vec![],
        },
        Case {
            name: "a move onto another file's path displaces it",
            rows: vec![
                row(1, "/lib/a.mkv", 10),
                row(2, "/lib/b.mkv", 20),
                row(3, "/lib/c.mkv", 30),
            ],
            walked: vec![],
            // a -> b over b's file, then c -> a.
            fingerprints: vec![("/lib/a.mkv", 30), ("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv"), to(3, "/lib/a.mkv")],
            displaced: vec![2],
        },
        Case {
            name: "a rename onto the path of a row already missing",
            rows: vec![
                row(1, "/lib/heat.mkv", 10),
                MediaFile {
                    missing_since: Some(instant(0)),
                    ..row(2, "/lib/stale.mkv", 20)
                },
            ],
            walked: vec![],
            fingerprints: vec![("/lib/stale.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/stale.mkv")],
            displaced: vec![2],
        },
        Case {
            name: "a copy leaves the row whose path still holds it",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec!["/lib/a.mkv"],
            fingerprints: vec![("/lib/copy.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a missing row back at its own path is restored, not moved",
            rows: vec![MediaFile {
                missing_since: Some(instant(0)),
                ..row(1, "/lib/a.mkv", 10)
            }],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "new content no row records is a change, and moves nothing",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 99)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a row under a path the walk could not read is not moved",
            rows: vec![row(1, "/lib/unreadable/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec!["/lib/unreadable"],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a walk that failed without a path moves nothing",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec!["/"],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a row never hashed cannot be followed",
            rows: vec![row(1, "/lib/a.mkv", 0)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 0)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "the same hash at another size is other content",
            rows: vec![MediaFile {
                size_bytes: 999,
                ..row(1, "/lib/a.mkv", 10)
            }],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "two identical files moved together keep their names",
            rows: vec![row(1, "/lib/x/One.mkv", 10), row(2, "/lib/x/Two.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/y/Two.mkv", 10), ("/lib/y/One.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/y/One.mkv"), to(2, "/lib/y/Two.mkv")],
            displaced: vec![],
        },
    ];
    for Case {
        name,
        rows,
        walked,
        fingerprints,
        shielded,
        relinks,
        displaced,
    } in cases
    {
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            assert_eq!(
                moves(&rows, &walked, &fingerprints, &shielded, &none),
                (relinks.clone(), displaced.clone()),
                "{name}"
            );
        }
    }
}

/// Two identical copies, one moved and one deleted: whichever the scan
/// meets, the row someone has been watching keeps the file.
#[test]
fn of_identical_copies_the_row_played_most_recently_follows_the_file() {
    let rows = [row(1, "/lib/a/Film.mkv", 10), row(2, "/lib/b/Film.mkv", 10)];
    for (watched, other) in [(1u128, 2u128), (2, 1)] {
        let last_played: HashMap<Uuid, DateTime<Utc>> =
            [(Uuid::from_u128(watched), instant(5))].into();
        assert_eq!(
            moves(&rows, &[], &[("/lib/c/Film.mkv", 10)], &[], &last_played),
            (vec![to(watched, "/lib/c/Film.mkv")], vec![]),
            "the other, {other}, is left to be marked missing"
        );
    }
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
    progress: Arc<InMemoryPlaybackProgressRepository>,
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
        let clock = Arc::new(TestClock::starting_at(instant(0)));
        let progress = Arc::new(InMemoryPlaybackProgressRepository::new(
            clock.clone(),
            file_repo.clone(),
        ));
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
            progress.clone(),
        )
        .with_clock(clock);
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
            progress,
            service,
        }
    }

    /// Record that someone watched `path`'s file.
    async fn watch(&self, path: &Path) {
        let file = self.present(path).await;
        self.progress
            .upsert(UpsertPlaybackProgress {
                user_id: Uuid::new_v4(),
                file_id: file.id,
                position_secs: 600.0,
                duration_secs: Some(3600.0),
            })
            .await
            .unwrap();
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
        Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
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

// ─── swaps, rotations, and paths taken over (issue #180) ─────────────────────

/// Two files that swap names -- through a temporary name, as `mv` does it --
/// each keep their own row: its playback progress stays on its content,
/// not on its old name.
#[tokio::test]
async fn two_files_that_swap_names_keep_their_own_rows() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat, a film about a heist");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let (heat_id, ronin_id) = (h.present(&heat).await.id, h.present(&ronin).await.id);
    let probes = h.probes();

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (2, 0, 0)
    );
    assert_eq!(
        h.present(&ronin).await.id,
        heat_id,
        "Heat's row is at its bytes"
    );
    assert_eq!(h.present(&heat).await.id, ronin_id);
    assert_eq!(h.probes(), probes, "nothing was probed");
    assert_eq!(h.row_count().await, 2);
}

/// Three files rotated among three names each keep their own row.
#[tokio::test]
async fn three_files_rotated_among_their_names_keep_their_own_rows() {
    let h = Harness::new().await;
    let a = h.write("A.mkv", "first");
    let b = h.write("B.mkv", "second, longer");
    let c = h.write("C.mkv", "third, longest of all");
    h.scan().await;
    let ids = [
        h.present(&a).await.id,
        h.present(&b).await.id,
        h.present(&c).await.id,
    ];

    let tmp = h.mv(&a, "tmp.mkv");
    h.mv(&c, "A.mkv");
    h.mv(&b, "C.mkv");
    h.mv(&tmp, "B.mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.changed), (3, 0));
    assert_eq!(h.present(&b).await.id, ids[0]);
    assert_eq!(h.present(&c).await.id, ids[1]);
    assert_eq!(h.present(&a).await.id, ids[2]);
    assert_eq!(h.row_count().await, 3);
}

/// A file renamed onto the path of a file deleted earlier, whose row is
/// still missing, keeps its own row and its progress; the stale row is not
/// restored with the renamed file's content, but kept aside as missing.
#[tokio::test]
async fn a_file_renamed_onto_a_missing_rows_path_keeps_its_own_row() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let stale = h.write("Old (1990).mkv", "an old film");
    h.scan().await;
    h.watch(&heat).await;
    let heat_id = h.present(&heat).await.id;
    let stale_id = h.present(&stale).await.id;
    std::fs::remove_file(&stale).unwrap();
    h.scan().await;
    assert!(h.row(&stale).await.unwrap().missing_since.is_some());

    h.mv(&heat, "Old (1990).mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.restored, progress.changed),
        (1, 0, 0)
    );
    assert_eq!(h.present(&stale).await.id, heat_id);
    assert!(
        h.row(&heat).await.is_none(),
        "nothing left at Heat's old name"
    );
    let kept = h
        .file_repo
        .find_all_by_library_including_missing(h.library.id)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == stale_id)
        .expect("the stale row is kept, not deleted");
    assert!(kept.missing_since.is_some());
    assert_eq!(
        h.progress
            .last_played_at(vec![heat_id])
            .await
            .unwrap()
            .len(),
        1,
        "Heat's progress is still Heat's"
    );
}

/// A row kept aside when another file took its path is still its file's:
/// brought back under a new name, the file is relinked to it. The move is
/// reported from the path the file had -- never from the name the row was
/// kept at, which no file ever had.
#[tokio::test]
async fn a_displaced_row_found_again_is_reported_from_its_own_path() {
    let h = Harness::new().await;
    let old = h.write("Old (1990).mkv", "an old film");
    let heat = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let old_id = h.present(&old).await.id;
    let away = h.dir.path().join("outside").join("Old (1990).mkv");
    std::fs::rename(&old, &away).unwrap();
    h.scan().await;
    h.mv(&heat, "Old (1990).mkv");
    h.scan().await;
    assert_ne!(h.present(&old).await.id, old_id, "Heat took Old's path");

    let back = h.root.join("Old back (1990).mkv");
    std::fs::rename(&away, &back).unwrap();
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&back).await.id, old_id);
    let found_again = h
        .move_logs()
        .await
        .into_iter()
        .find(|log| log.details.as_ref().unwrap()["file_id"] == old_id.to_string())
        .expect("the move is logged");
    assert_eq!(
        found_again.details.as_ref().unwrap()["from"],
        serde_json::json!(old.display().to_string())
    );
    assert_eq!(
        found_again.message,
        format!(
            "File moved: '{}' is now '{}'",
            old.display(),
            back.display()
        )
    );
    let logged = h.admin_log_repo.list(100, 0).await.unwrap();
    let published = h.notifications.published_events();
    let told = logged
        .iter()
        .map(|log| format!("{} {:?}", log.message, log.details))
        .chain(published.iter().map(|event| event.message.clone()));
    for text in told {
        assert!(!text.contains(".beam-displaced-"), "{text}");
    }
}

/// One of two identical copies moved, the other deleted: the row someone
/// was watching keeps the file, and the other is marked missing.
#[tokio::test]
async fn of_two_identical_copies_the_watched_row_keeps_the_moved_file() {
    for watch_first in [true, false] {
        let h = Harness::new().await;
        let first = h.write("First/Film.mkv", "the same film");
        let second = h.write("Second/Film.mkv", "the same film");
        h.scan().await;
        let (watched, other) = if watch_first {
            (&first, &second)
        } else {
            (&second, &first)
        };
        h.watch(watched).await;
        let watched_id = h.present(watched).await.id;
        let other_id = h.present(other).await.id;

        h.mv(&first, "Moved/Film.mkv");
        std::fs::remove_file(&second).unwrap();
        let progress = h.scan().await;

        assert_eq!((progress.relinked, progress.marked_missing), (1, 1));
        assert_eq!(
            h.present(&h.root.join("Moved/Film.mkv")).await.id,
            watched_id,
            "watched first: {watch_first}"
        );
        let missing = h
            .file_repo
            .find_all_by_library_including_missing(h.library.id)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == other_id)
            .unwrap();
        assert!(missing.missing_since.is_some());
    }
}

/// A watcher event cannot tell a swap from a change: the two halves arrive
/// apart. A file whose new content is another file's that may have moved is
/// left as it is -- not restored, not marked changed -- and the next scan,
/// which sees both paths, gives each row back its content.
#[tokio::test]
async fn a_swap_seen_by_the_watcher_is_left_to_the_scan() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat, a film about a heist");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let (heat_row, ronin_row) = (h.present(&heat).await, h.present(&ronin).await);

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    for path in [&heat, &ronin, &tmp] {
        let kind = if path == &tmp {
            FsEventKind::Removed
        } else {
            FsEventKind::Modified
        };
        assert_eq!(h.reconcile(path, kind).await, ReconcileOutcome::Done);
    }

    let (at_heat, at_ronin) = (h.present(&heat).await, h.present(&ronin).await);
    let recorded = |row: &MediaFile| {
        (
            row.id,
            row.hash,
            row.size_bytes,
            row.mtime,
            row.status,
            row.duration,
            row.missing_since,
        )
    };
    assert_eq!(
        recorded(&at_heat),
        recorded(&heat_row),
        "left exactly as it was"
    );
    assert_eq!(recorded(&at_ronin), recorded(&ronin_row));
    assert!(
        h.row(&tmp).await.is_none(),
        "the passing name is not indexed"
    );

    let progress = h.scan().await;
    assert_eq!((progress.relinked, progress.changed), (2, 0));
    assert_eq!(h.present(&ronin).await.id, heat_row.id);
    assert_eq!(h.present(&heat).await.id, ronin_row.id);
}

/// A new name whose content is a file whose own path now holds other
/// content -- one half of a rotation -- is left to the scan by the watcher,
/// rather than indexed as a copy beside the row it belongs to.
#[tokio::test]
async fn a_new_name_holding_a_file_whose_path_changed_is_left_to_the_scan() {
    let h = Harness::new().await;
    let a = h.write("A.mkv", "first");
    let c = h.write("C.mkv", "third, longer");
    h.scan().await;
    let a_id = h.present(&a).await.id;

    let n = h.mv(&a, "N.mkv");
    h.mv(&c, "A.mkv");
    assert_eq!(
        h.reconcile(&n, FsEventKind::Created).await,
        ReconcileOutcome::Done
    );
    assert!(h.row(&n).await.is_none(), "not indexed as a copy");

    let progress = h.scan().await;
    assert_eq!((progress.relinked, progress.added), (2, 0));
    assert_eq!(h.present(&n).await.id, a_id);
}
