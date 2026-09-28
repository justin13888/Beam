//! The indexer reads a media file only with no link followed beneath the
//! library root (issue #238): its stat without opening it, and its content
//! hash and its probe from one handle opened the same way.
//!
//! The filesystem is a real `TempDir`, the hasher and prober the real ones,
//! over committed media fixtures, so what a row records can be told apart:
//! the library holds an H.264 file, and outside it waits an HEVC file of the
//! same name. The window between a scan's walk and its reads is reached
//! through the one seam a scan asks after its walk and before it reads a file
//! -- the [`FilesystemProbe`] classifying the library's filesystem -- which
//! here swaps a file, or a folder above it, for a link to the outside. A
//! later window, between one file's hash and the next file's stat, is reached
//! through the hasher, which runs a swap before it hashes.

use std::sync::Mutex;

use super::*;
use crate::services::admin_log::in_memory::NoOpAdminLogService;
use crate::services::filesystem_probe::FilesystemProbe;
use crate::services::hash::HashService;
use crate::services::hash::LocalHashService;
use crate::services::media_info::LocalMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::CreateLibrary;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

/// The bytes of a committed fixture under `beam-index/tests/fixtures/`.
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// What the library holds, and what waits outside it under the same name.
const INSIDE: &str = "h264.mkv";
const OUTSIDE: &str = "hevc.mkv";

/// A local filesystem whose classification -- asked once a scan has walked
/// the root and before it reads a file -- runs the swap a test arms.
#[derive(Default)]
struct SwapAfterWalk {
    swap: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for SwapAfterWalk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SwapAfterWalk")
    }
}

impl FilesystemProbe for SwapAfterWalk {
    fn kind(&self, _path: &Path) -> std::io::Result<FilesystemKind> {
        if let Some(swap) = self.swap.lock().unwrap().take() {
            swap();
        }
        Ok(FilesystemKind::Local)
    }
}

/// The real hasher, running the swap a test arms before the next hash.
#[derive(Debug, Default)]
struct SwapBeforeHash {
    inner: LocalHashService,
    swap: SwapAfterWalk,
}

impl SwapBeforeHash {
    fn run_swap(&self) {
        if let Some(swap) = self.swap.swap.lock().unwrap().take() {
            swap();
        }
    }
}

#[async_trait::async_trait]
impl HashService for SwapBeforeHash {
    fn hash_sync(&self, file: std::fs::File) -> std::io::Result<u64> {
        self.run_swap();
        self.inner.hash_sync(file)
    }

    async fn hash_async(&self, file: std::fs::File) -> std::io::Result<u64> {
        self.run_swap();
        self.inner.hash_async(file).await
    }
}

struct Harness {
    dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    stream_repo: Arc<InMemoryMediaStreamRepository>,
    sidecar_repo: Arc<InMemorySidecarSubtitleRepository>,
    probe: Arc<SwapAfterWalk>,
    hasher: Arc<SwapBeforeHash>,
    clock: Arc<TestClock>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        crate::probe::init().unwrap();
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
        let library = library_repo
            .create(CreateLibrary {
                name: "No links".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
        let probe = Arc::new(SwapAfterWalk::default());
        let hasher = Arc::new(SwapBeforeHash::default());
        let clock = Arc::new(TestClock::starting_at(Utc::now()));
        let service = LocalIndexService::new(
            library_repo,
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
            stream_repo.clone(),
            hasher.clone(),
            Arc::new(LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(InMemoryWatchStateRepository::default()),
        )
        .with_filesystem_probe(probe.clone())
        .with_sidecar_repo(sidecar_repo.clone())
        .with_clock(clock.clone());
        Self {
            dir,
            root,
            library,
            file_repo,
            stream_repo,
            sidecar_repo,
            probe,
            hasher,
            clock,
            service,
        }
    }

    /// Put the inside fixture at `rel` under the root.
    fn put_inside(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fixture(INSIDE)).unwrap();
        path
    }

    /// Put the outside fixture at `rel` under the folder outside the root.
    fn put_outside(&self, rel: &str) -> PathBuf {
        let path = self.outside().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fixture(OUTSIDE)).unwrap();
        path
    }

    fn outside(&self) -> PathBuf {
        self.dir.path().join("outside")
    }

    /// Run `swap` between the next scan's walk and its first read.
    fn after_the_walk(&self, swap: impl FnOnce() + Send + 'static) {
        *self.probe.swap.lock().unwrap() = Some(Box::new(swap));
    }

    /// Run `swap` just before the next file is hashed.
    fn before_the_next_hash(&self, swap: impl FnOnce() + Send + 'static) {
        *self.hasher.swap.swap.lock().unwrap() = Some(Box::new(swap));
    }

    async fn scan(&self) {
        self.service
            .scan_library(self.library.id.to_string())
            .await
            .unwrap();
    }

    async fn row(&self, path: &Path) -> Option<MediaFile> {
        self.file_repo
            .find_all_by_library_including_missing(self.library.id)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.path == path)
    }
}

/// Replace `path` with a link to `target`.
#[cfg(unix)]
fn swap_for_link(path: &Path, target: &Path) {
    if path.is_dir() {
        std::fs::remove_dir_all(path).unwrap();
    } else {
        std::fs::remove_file(path).unwrap();
    }
    std::os::unix::fs::symlink(target, path).unwrap();
}

/// A file read through no link is recorded from its own bytes: the hash is
/// the digest of exactly those bytes, and the probe reads the container they
/// hold.
#[tokio::test]
async fn a_regular_file_is_hashed_and_probed_from_its_own_bytes() {
    let h = Harness::new().await;
    let path = h.put_inside("Heat (1995)/Heat (1995).mkv");

    h.scan().await;

    let row = h.row(&path).await.expect("the file is indexed");
    assert_eq!(
        row.hash,
        beam_domain::utils::hash::compute_hash(&fixture(INSIDE)[..]).unwrap()
    );
    assert_eq!(row.size_bytes, fixture(INSIDE).len() as u64);
    assert_eq!(row.container_format.as_deref(), Some("matroska,webm"));
    let streams = h.stream_repo.find_by_file_id(row.id).await.unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec, "h264");
}

/// A new file swapped for a link to an outside file of the same name after
/// the walk listed it: nothing is recorded for it -- not the outside file's
/// hash, size or streams.
#[cfg(unix)]
#[tokio::test]
async fn a_file_swapped_for_a_link_after_the_walk_is_not_recorded() {
    let h = Harness::new().await;
    let path = h.put_inside("Heat (1995).mkv");
    let outside = h.put_outside("Heat (1995).mkv");
    let swapped = path.clone();
    h.after_the_walk(move || swap_for_link(&swapped, &outside));

    h.scan().await;

    assert!(
        h.row(&path).await.is_none(),
        "nothing read through the link"
    );
}

/// A folder above a new file swapped for a link to an outside folder that
/// holds a file of the same name: nothing is recorded for it.
#[cfg(unix)]
#[tokio::test]
async fn a_folder_swapped_for_a_link_after_the_walk_is_not_followed() {
    let h = Harness::new().await;
    let path = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.put_outside("Heat (1995)/Heat (1995).mkv");
    let folder = h.root.join("Heat (1995)");
    let outside = h.outside().join("Heat (1995)");
    h.after_the_walk(move || swap_for_link(&folder, &outside));

    h.scan().await;

    assert!(
        h.row(&path).await.is_none(),
        "nothing read through the link"
    );
}

/// An indexed file whose folder is swapped for a link after the walk, and
/// whose content changed as it went: its row is treated as missing, as a link
/// the walk had seen is, and keeps the hash, size and streams of the file it
/// recorded -- nothing of the outside file it would now lead to.
#[cfg(unix)]
#[tokio::test]
async fn an_indexed_file_behind_a_folder_swapped_for_a_link_is_missing_and_keeps_its_record() {
    let h = Harness::new().await;
    let path = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&path).await.expect("the file is indexed");
    let streams_before = h.stream_repo.find_by_file_id(before.id).await.unwrap();

    h.put_outside("Heat (1995)/Heat (1995).mkv");
    let folder = h.root.join("Heat (1995)");
    let outside = h.outside().join("Heat (1995)");
    h.after_the_walk(move || swap_for_link(&folder, &outside));
    h.scan().await;

    let after = h.row(&path).await.expect("the row is kept");
    assert!(after.missing_since.is_some(), "treated as missing");
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.size_bytes, before.size_bytes);
    assert_eq!(after.identity, before.identity);
    let codecs = |streams: Vec<beam_domain::models::MediaStream>| {
        streams
            .into_iter()
            .map(|stream| (stream.id, stream.codec))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        codecs(h.stream_repo.find_by_file_id(after.id).await.unwrap()),
        codecs(streams_before)
    );
}

/// The same for an indexed file itself swapped for a link after the walk.
#[cfg(unix)]
#[tokio::test]
async fn an_indexed_file_swapped_for_a_link_after_the_walk_is_missing_and_keeps_its_record() {
    let h = Harness::new().await;
    let path = h.put_inside("Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&path).await.expect("the file is indexed");

    let outside = h.put_outside("Heat (1995).mkv");
    let swapped = path.clone();
    h.after_the_walk(move || swap_for_link(&swapped, &outside));
    h.scan().await;

    let after = h.row(&path).await.expect("the row is kept");
    assert!(after.missing_since.is_some(), "treated as missing");
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.size_bytes, before.size_bytes);
}

/// A watcher event for a file whose folder has become a link: the stat the
/// event takes follows the folder, but the file is no library file, so a new
/// one is not indexed and an indexed one is marked missing.
#[cfg(unix)]
#[tokio::test]
async fn a_watcher_event_behind_a_folder_link_indexes_nothing_and_hides_the_row() {
    let h = Harness::new().await;
    let indexed = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&indexed).await.expect("the file is indexed");
    h.put_outside("Heat (1995)/Heat (1995).mkv");
    h.put_outside("Heat (1995)/Heat (1995) - Extended.mkv");
    swap_for_link(
        &h.root.join("Heat (1995)"),
        &h.outside().join("Heat (1995)"),
    );

    let new = h.root.join("Heat (1995)/Heat (1995) - Extended.mkv");
    h.service
        .reconcile_path(h.library.id, new.clone(), FsEventKind::Created)
        .await
        .unwrap();
    h.service
        .reconcile_path(h.library.id, indexed.clone(), FsEventKind::Modified)
        .await
        .unwrap();

    assert!(h.row(&new).await.is_none(), "nothing read through the link");
    let after = h.row(&indexed).await.expect("the row is kept");
    assert!(after.missing_since.is_some(), "treated as missing");
    assert_eq!(after.hash, before.hash);
}

/// A directory event beneath a folder that has become a link: the stat the
/// event takes follows the link, but the directory's files are no library
/// files, so the row the file's own event marked missing is not restored --
/// and nothing of the outside file is recorded against it.
#[cfg(unix)]
#[tokio::test]
async fn a_directory_event_behind_a_folder_link_does_not_restore_the_row() {
    let h = Harness::new().await;
    let indexed = h.put_inside("Show/Season 01/Show - S01E01.mkv");
    h.scan().await;
    let before = h.row(&indexed).await.expect("the file is indexed");
    h.put_outside("Show/Season 01/Show - S01E01.mkv");
    swap_for_link(&h.root.join("Show"), &h.outside().join("Show"));

    h.service
        .reconcile_path(h.library.id, indexed.clone(), FsEventKind::Modified)
        .await
        .unwrap();
    let marked = h.row(&indexed).await.expect("the row is kept");
    assert!(
        marked.missing_since.is_some(),
        "the file event hides the row"
    );
    h.clock.advance(Duration::from_secs(60));
    h.service
        .reconcile_path(
            h.library.id,
            h.root.join("Show/Season 01"),
            FsEventKind::Modified,
        )
        .await
        .unwrap();

    let after = h.row(&indexed).await.expect("the row is kept");
    assert_eq!(
        after.missing_since, marked.missing_since,
        "not restored, nor marked again"
    );
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.size_bytes, before.size_bytes);
}

/// A file the watcher reconciles whose folder has become a link is marked
/// missing before anything restores it: a row already missing keeps its
/// first stamp rather than being restored and stamped again.
#[cfg(unix)]
#[tokio::test]
async fn reconciling_a_file_behind_a_folder_link_marks_it_missing_without_restoring_it() {
    let h = Harness::new().await;
    let indexed = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.scan().await;
    let row = h.row(&indexed).await.expect("the file is indexed");
    let first_seen_gone = h.clock.now();
    h.file_repo
        .mark_missing(vec![row.id], first_seen_gone)
        .await
        .unwrap();
    h.put_outside("Heat (1995)/Heat (1995).mkv");
    swap_for_link(
        &h.root.join("Heat (1995)"),
        &h.outside().join("Heat (1995)"),
    );
    h.clock.advance(Duration::from_secs(60));

    h.service
        .reconcile_file(&indexed, &h.library, false, Inodes::Stable)
        .await
        .unwrap();

    let after = h.row(&indexed).await.expect("the row is kept");
    assert_eq!(after.missing_since, Some(first_seen_gone));
    assert_eq!(after.hash, row.hash);
}

/// An indexed file whose folder becomes a link after the scan has stat'ed
/// every walked file -- while it hashes another that changed -- is marked
/// missing when the scan reaches it, not left present.
#[cfg(unix)]
#[tokio::test]
async fn an_indexed_file_whose_folder_becomes_a_link_mid_scan_is_marked_missing() {
    let h = Harness::new().await;
    let changed = h.put_inside("Heat (1995)/Heat (1995).mkv");
    let indexed = h.put_inside("Ronin (1998)/Ronin (1998).mkv");
    h.scan().await;
    let before = h.row(&indexed).await.expect("the file is indexed");

    let mut bytes = fixture(INSIDE);
    bytes.extend_from_slice(b"appended");
    std::fs::write(&changed, bytes).unwrap();
    h.put_outside("Ronin (1998)/Ronin (1998).mkv");
    let folder = h.root.join("Ronin (1998)");
    let outside = h.outside().join("Ronin (1998)");
    h.before_the_next_hash(move || swap_for_link(&folder, &outside));
    h.scan().await;

    let after = h.row(&indexed).await.expect("the row is kept");
    assert!(after.missing_since.is_some(), "treated as missing");
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.size_bytes, before.size_bytes);
}

/// A folder renamed away and replaced by a link while the scan's
/// file-by-file pass hashes a new file in it. The pass may already hold the
/// folder open from an earlier file, and then reads the folder's other files
/// as they were (the renamed folder), so their rows can stay present for this
/// scan; or it opens the folder anew and refuses the link. Either way nothing
/// of the outside folder is recorded, and the next scan's walk refuses the
/// link and marks every row in it missing.
#[cfg(unix)]
#[tokio::test]
async fn a_folder_swapped_for_a_link_during_the_pass_records_nothing_from_outside() {
    let h = Harness::new().await;
    let anchor = h.root.join("Ronin (1998)/Ronin (1998).mkv");
    std::fs::create_dir_all(anchor.parent().unwrap()).unwrap();
    let mut anchor_bytes = fixture(INSIDE);
    anchor_bytes.extend_from_slice(b"another film");
    std::fs::write(&anchor, anchor_bytes).unwrap();
    let indexed = [
        h.put_inside("Heat (1995)/a.mkv"),
        h.put_inside("Heat (1995)/c.mkv"),
    ];
    h.scan().await;

    let new = h.put_inside("Heat (1995)/b.mkv");
    for name in ["a.mkv", "b.mkv", "c.mkv"] {
        h.put_outside(&format!("Heat (1995)/{name}"));
    }
    let folder = h.root.join("Heat (1995)");
    let (swapped, renamed, outside) = (
        folder.clone(),
        h.dir.path().join("renamed away"),
        h.outside().join("Heat (1995)"),
    );
    h.before_the_next_hash(move || {
        std::fs::rename(&swapped, &renamed).unwrap();
        std::os::unix::fs::symlink(&outside, &swapped).unwrap();
    });
    h.scan().await;
    assert!(
        folder.is_symlink(),
        "the new file was hashed, and the swap ran"
    );

    let inside_hash = beam_domain::utils::hash::compute_hash(&fixture(INSIDE)[..]).unwrap();
    let in_folder = [&indexed[0], &new, &indexed[1]];
    for path in in_folder {
        let Some(row) = h.row(path).await else {
            continue;
        };
        assert_eq!(row.hash, inside_hash, "{}", path.display());
        assert_eq!(row.size_bytes, fixture(INSIDE).len() as u64);
        let streams = h.stream_repo.find_by_file_id(row.id).await.unwrap();
        assert!(
            streams.iter().all(|stream| stream.codec == "h264"),
            "{}",
            path.display()
        );
    }

    h.scan().await;

    for path in &indexed {
        let row = h.row(path).await.expect("the row is kept");
        assert!(row.missing_since.is_some(), "{} is missing", path.display());
        assert_eq!(row.hash, inside_hash);
    }
    if let Some(row) = h.row(&new).await {
        assert!(row.missing_since.is_some(), "the new file is missing");
    }
    let anchor_row = h.row(&anchor).await.expect("the other film is indexed");
    assert!(anchor_row.missing_since.is_none());
}

/// A video relinked by the watcher drops the record of a subtitle it had
/// whose folder has since become a link: that path leads to no file of the
/// library, just as a watcher event on it would find.
#[cfg(unix)]
#[tokio::test]
async fn a_relinked_video_drops_a_subtitle_behind_a_folder_link() {
    let h = Harness::new().await;
    let video = h.put_inside("Heat (1995)/Heat (1995).mkv");
    let subtitle = h.root.join("Heat (1995)/Heat (1995).en.srt");
    std::fs::write(&subtitle, b"1\n00:00:01,000 --> 00:00:02,000\nInside\n").unwrap();
    h.scan().await;
    assert!(
        h.sidecar_repo
            .find_by_path(&subtitle)
            .await
            .unwrap()
            .is_some(),
        "the subtitle is recorded"
    );

    let moved = h.root.join("Moved/Heat (1995).mkv");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&video, &moved).unwrap();
    let outside = h.outside().join("Heat (1995)");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("Heat (1995).en.srt"),
        b"1\n00:00:01,000 --> 00:00:02,000\nOutside\n",
    )
    .unwrap();
    swap_for_link(&h.root.join("Heat (1995)"), &outside);

    let outcome = h
        .service
        .reconcile_path(h.library.id, moved.clone(), FsEventKind::Created)
        .await
        .unwrap();

    assert!(
        h.row(&moved).await.is_some(),
        "the video is relinked: {outcome:?}"
    );
    assert!(
        h.sidecar_repo
            .find_by_path(&subtitle)
            .await
            .unwrap()
            .is_none(),
        "no file of the library is there"
    );
}

/// Whether this process is refused what a file's permissions refuse: not
/// when it runs as root, which is refused nothing.
#[cfg(unix)]
fn permissions_are_enforced(path: &Path) -> bool {
    let original = std::fs::metadata(path).unwrap().permissions();
    chmod(path, 0o000);
    let enforced = std::fs::File::open(path).is_err();
    std::fs::set_permissions(path, original).unwrap();
    if !enforced {
        eprintln!("skipped: this process is not refused by file permissions (running as root)");
    }
    enforced
}

/// Set `path`'s permission bits to `mode`.
#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A walked file whose stat then fails for a reason other than a link --
/// its folder made unsearchable after the walk, so `EACCES` -- keeps its
/// row as it is: the failure says nothing about the file (issue #179).
#[cfg(unix)]
#[tokio::test]
async fn a_walked_file_that_then_fails_to_stat_keeps_its_row() {
    let h = Harness::new().await;
    let indexed = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&indexed).await.expect("the file is indexed");
    if !permissions_are_enforced(&indexed) {
        return;
    }
    let folder = h.root.join("Heat (1995)");
    let locked = folder.clone();
    h.after_the_walk(move || chmod(&locked, 0o600));

    h.scan().await;
    chmod(&folder, 0o755);

    let after = h.row(&indexed).await.expect("the row is kept");
    assert_eq!(after.missing_since, None, "not treated as missing");
    assert_eq!(after.hash, before.hash);
}

/// A changed file that stats but cannot be opened (mode 000, so `EACCES`)
/// keeps its row as it is: neither missing nor rehashed (issue #179).
#[cfg(unix)]
#[tokio::test]
async fn a_changed_file_that_cannot_be_opened_keeps_its_row() {
    let h = Harness::new().await;
    let indexed = h.put_inside("Heat (1995)/Heat (1995).mkv");
    h.scan().await;
    let before = h.row(&indexed).await.expect("the file is indexed");
    if !permissions_are_enforced(&indexed) {
        return;
    }
    let mut bytes = fixture(INSIDE);
    bytes.extend_from_slice(b"appended");
    std::fs::write(&indexed, bytes).unwrap();
    chmod(&indexed, 0o000);

    h.scan().await;
    chmod(&indexed, 0o644);

    let after = h.row(&indexed).await.expect("the row is kept");
    assert_eq!(after.missing_since, None, "not treated as missing");
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.size_bytes, before.size_bytes);
}

/// A subtitle event whose folder has become a link to an outside folder
/// holding a subtitle of the same name: the subtitle is no file of the
/// library, so its record is dropped -- and nothing of the outside subtitle
/// is recorded in its place.
#[cfg(unix)]
#[tokio::test]
async fn a_subtitle_event_behind_a_folder_link_records_nothing_of_the_outside_file() {
    let h = Harness::new().await;
    h.put_inside("Heat (1995)/Heat (1995).mkv");
    let subtitle = h.root.join("Heat (1995)/Heat (1995).en.srt");
    std::fs::write(&subtitle, b"1\n00:00:01,000 --> 00:00:02,000\nInside\n").unwrap();
    h.scan().await;
    assert!(
        h.sidecar_repo
            .find_by_path(&subtitle)
            .await
            .unwrap()
            .is_some(),
        "the subtitle is recorded"
    );

    let outside = h.outside().join("Heat (1995)");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Heat (1995).mkv"), fixture(OUTSIDE)).unwrap();
    std::fs::write(
        outside.join("Heat (1995).en.srt"),
        b"1\n00:00:01,000 --> 00:00:02,000\nA much longer outside subtitle\n",
    )
    .unwrap();
    swap_for_link(&h.root.join("Heat (1995)"), &outside);

    h.service
        .reconcile_path(h.library.id, subtitle.clone(), FsEventKind::Modified)
        .await
        .unwrap();

    assert!(
        h.sidecar_repo
            .find_by_path(&subtitle)
            .await
            .unwrap()
            .is_none(),
        "no file of the library is there"
    );
}

/// A walk from a folder the watcher names beneath a folder that has become
/// a link -- the walk's own start reached through the link -- lists nothing
/// of what is there: no media, no subtitle and no NFO, whose stats it would
/// otherwise record, and no failure either. Each entry is stat'ed beneath
/// the root with no link followed.
#[cfg(unix)]
#[test]
fn a_walk_beneath_a_folder_link_lists_nothing_through_it() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("library");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(root.join("Show")).unwrap();
    std::fs::create_dir_all(outside.join("Season 01")).unwrap();
    for name in ["Show - S01E01.mkv", "Show - S01E01.en.srt", "tvshow.nfo"] {
        std::fs::write(outside.join("Season 01").join(name), b"outside").unwrap();
    }
    std::os::unix::fs::symlink(&outside, root.join("Show/Linked")).unwrap();

    let WalkOutcome {
        files,
        video_files_seen,
        excluded,
        failed_subtrees,
        unscoped_failure,
        subtitles,
        nfos,
    } = walk_under(
        &root,
        &root.join("Show/Linked/Season 01"),
        &PathPolicy::default(),
    );

    assert_eq!(files, Vec::<PathBuf>::new());
    assert_eq!(video_files_seen, 0);
    assert_eq!(excluded, 0);
    assert_eq!(failed_subtrees, Vec::<PathBuf>::new());
    assert!(!unscoped_failure);
    assert!(subtitles.is_empty());
    assert!(nfos.is_empty());
}
