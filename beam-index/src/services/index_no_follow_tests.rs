//! The indexer reads a media file only through the no-follow opener (issue
//! #238): its stat, its content hash and its probe all come from one handle
//! opened beneath the library root with no link followed.
//!
//! The filesystem is a real `TempDir`, the hasher and prober the real ones,
//! over committed media fixtures, so what a row records can be told apart:
//! the library holds an H.264 file, and outside it waits an HEVC file of the
//! same name. The window between a scan's walk and its reads is reached
//! through the one seam a scan asks after its walk and before it reads a file
//! -- the [`FilesystemProbe`] classifying the library's filesystem -- which
//! here swaps a file, or a folder above it, for a link to the outside.

use std::sync::Mutex;

use super::*;
use crate::services::admin_log::in_memory::NoOpAdminLogService;
use crate::services::filesystem_probe::FilesystemProbe;
use crate::services::hash::LocalHashService;
use crate::services::media_info::LocalMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::CreateLibrary;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
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

struct Harness {
    dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    stream_repo: Arc<InMemoryMediaStreamRepository>,
    probe: Arc<SwapAfterWalk>,
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
        let probe = Arc::new(SwapAfterWalk::default());
        let service = LocalIndexService::new(
            library_repo,
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
            stream_repo.clone(),
            Arc::new(LocalHashService::default()),
            Arc::new(LocalMediaInfoService::default()),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(NoOpAdminLogService),
            Arc::new(InMemoryPlaybackProgressRepository::default()),
        )
        .with_filesystem_probe(probe.clone());
        Self {
            dir,
            root,
            library,
            file_repo,
            stream_repo,
            probe,
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
