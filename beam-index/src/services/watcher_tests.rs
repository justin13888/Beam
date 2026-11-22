use super::*;

fn event(library_id: Uuid, path: &str, kind: FsEventKind) -> FsEvent {
    FsEvent {
        library_id,
        path: PathBuf::from(path),
        kind,
    }
}

#[test]
fn test_translate_event_kind() {
    use notify::EventKind;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode};

    assert_eq!(
        translate_event_kind(&EventKind::Create(CreateKind::File)),
        Some(FsEventKind::Created)
    );
    assert_eq!(
        translate_event_kind(&EventKind::Modify(ModifyKind::Any)),
        Some(FsEventKind::Modified)
    );
    assert_eq!(
        translate_event_kind(&EventKind::Remove(RemoveKind::File)),
        Some(FsEventKind::Removed)
    );
    assert_eq!(
        translate_event_kind(&EventKind::Access(AccessKind::Any)),
        None
    );
    // Either half of a rename, or both at once, is a change at the path:
    // the reconcile finds out which half it is on disk (issue #180).
    for mode in [RenameMode::From, RenameMode::To, RenameMode::Both] {
        assert_eq!(
            translate_event_kind(&EventKind::Modify(ModifyKind::Name(mode))),
            Some(FsEventKind::Modified),
            "{mode:?}"
        );
    }
}

#[test]
fn test_debouncer_coalesces_burst() {
    let lib = Uuid::new_v4();
    let mut debouncer = PathDebouncer::new();
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Created));
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Modified));
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Modified));

    let drained = debouncer.drain();
    assert_eq!(
        drained.len(),
        1,
        "a burst on one path collapses to one event"
    );
    assert_eq!(drained[0].kind, FsEventKind::Modified);
}

#[test]
fn test_debouncer_removed_supersedes_modified() {
    let lib = Uuid::new_v4();
    let mut debouncer = PathDebouncer::new();
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Modified));
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Removed));

    let drained = debouncer.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].kind, FsEventKind::Removed);
}

#[test]
fn test_debouncer_distinct_paths_independent() {
    let lib = Uuid::new_v4();
    let mut debouncer = PathDebouncer::new();
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Created));
    debouncer.submit(event(lib, "/media/b.mp4", FsEventKind::Created));
    assert_eq!(debouncer.drain().len(), 2);
}

#[test]
fn test_debouncer_drain_clears() {
    let lib = Uuid::new_v4();
    let mut debouncer = PathDebouncer::new();
    debouncer.submit(event(lib, "/media/a.mp4", FsEventKind::Created));
    assert_eq!(debouncer.drain().len(), 1);
    assert!(debouncer.is_empty());
    assert_eq!(debouncer.drain().len(), 0);
}

// ── NotifyFsWatcher over a real directory ───────────────────────────────
//
// The backends are real (`notify`'s inotify/FSEvents and poll watchers on a
// `TempDir`); only the filesystem *kind* is injected, because a `TempDir`
// cannot be put on NFS. Waits are bounded by a timeout rather than timed
// with a sleep: the event either arrives or the test fails.

use crate::services::filesystem_probe::FixedFilesystemProbe;

const EVENT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

fn notify_watcher(kind: FilesystemKind) -> (NotifyFsWatcher, Arc<WatchStatus>) {
    let status = Arc::new(WatchStatus::new());
    let watcher = NotifyFsWatcher::new(Arc::new(FixedFilesystemProbe(kind)), status.clone());
    (watcher, status)
}

/// The next event for `path`, skipping any for other paths (a create is
/// often followed by a modify of the same file, or of its directory).
async fn next_event_for(watcher: &NotifyFsWatcher, path: &Path) -> FsEvent {
    tokio::time::timeout(EVENT_DEADLINE, async {
        loop {
            let event = watcher.next_event().await.expect("watcher closed");
            if event.path == path {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no event for {}", path.display()))
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // macOS reports FSEvents paths under /private/var, not /var.
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

#[tokio::test]
async fn a_local_library_is_watched_natively() {
    let (_dir, root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();

    watcher.watch_library(lib, &root).unwrap();
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Native)
    );

    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    // No poll: a native watch delivers on its own.
    let event = next_event_for(&watcher, &file).await;
    assert_eq!(event.library_id, lib);
}

#[tokio::test]
async fn a_network_library_is_polled_and_a_poll_finds_a_new_file() {
    let (_dir, root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Network);
    let lib = Uuid::new_v4();

    watcher.watch_library(lib, &root).unwrap();
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Polling(PollReason::NetworkFilesystem))
    );

    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    watcher.poll_once();
    let event = next_event_for(&watcher, &file).await;
    assert_eq!(event.library_id, lib);
    assert_eq!(event.kind, FsEventKind::Created);
}

fn mounted_watcher(mounts: Vec<(PathBuf, FilesystemKind)>) -> (NotifyFsWatcher, Arc<WatchStatus>) {
    use crate::services::filesystem_probe::MountedFilesystemProbe;

    let status = Arc::new(WatchStatus::new());
    let watcher = NotifyFsWatcher::new(Arc::new(MountedFilesystemProbe { mounts }), status.clone());
    (watcher, status)
}

/// A share mounted inside a local root is changed by other hosts without
/// a native event, so the library is polled, and a poll finds a file
/// written into the share.
#[tokio::test]
async fn a_local_library_with_a_network_share_beneath_it_is_polled() {
    let (_dir, root) = canonical_tempdir();
    let share = root.join("tv").join("nas");
    std::fs::create_dir_all(&share).unwrap();
    let (watcher, status) = mounted_watcher(vec![(share.clone(), FilesystemKind::Network)]);
    let lib = Uuid::new_v4();

    let mode = watcher.watch_library(lib, &root).unwrap();
    assert_eq!(mode, WatchMode::Polling(PollReason::NetworkFilesystem));
    assert_eq!(status.snapshot().libraries.get(&lib), Some(&mode));

    let file = share.join("episode.mkv");
    std::fs::write(&file, b"x").unwrap();
    watcher.poll_once();
    let event = next_event_for(&watcher, &file).await;
    assert_eq!(event.library_id, lib);
    assert_eq!(event.kind, FsEventKind::Created);
}

/// Another local filesystem beneath the root, a network share beside
/// the root rather than beneath it, and a network share beneath only a
/// sibling library: none of them makes the library polled.
#[tokio::test]
async fn only_a_network_share_beneath_the_root_makes_a_library_polled() {
    let (_dir, parent) = canonical_tempdir();
    let root = parent.join("movies");
    let sibling = parent.join("movies-nas");
    let usb = root.join("usb");
    std::fs::create_dir_all(&usb).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let (watcher, status) = mounted_watcher(vec![
        (usb, FilesystemKind::Local),
        (sibling, FilesystemKind::Network),
    ]);
    let lib = Uuid::new_v4();

    assert_eq!(
        watcher.watch_library(lib, &root).unwrap(),
        WatchMode::Native
    );
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Native)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_poller_does_not_follow_a_symlinked_directory() {
    let (_dir, root) = canonical_tempdir();
    let (_outside_dir, outside) = canonical_tempdir();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    let (watcher, _status) = notify_watcher(FilesystemKind::Network);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();

    // A change behind the link and a marker inside the root, then a poll.
    // The poller scans on one thread, a poll at a time, so once the
    // marker shows the first poll has started, a second poll's fence is
    // reported only after the first poll has reported everything it
    // found: draining to the fence drains the first poll whole, rather
    // than stopping at the marker with the rest of its scan unread.
    std::fs::write(outside.join("behind-the-link.mkv"), b"x").unwrap();
    let marker = root.join("marker.mkv");
    std::fs::write(&marker, b"x").unwrap();
    watcher.poll_once();
    let drain_to = |stop: PathBuf| {
        let watcher = &watcher;
        async move {
            tokio::time::timeout(EVENT_DEADLINE, async {
                let mut seen = Vec::new();
                loop {
                    let event = watcher.next_event().await.expect("watcher closed");
                    let done = event.path == stop;
                    seen.push(event.path);
                    if done {
                        return seen;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("no poll reported {}", stop.display()))
        }
    };
    let mut seen = drain_to(marker).await;
    let fence = root.join("fence.mkv");
    std::fs::write(&fence, b"x").unwrap();
    watcher.poll_once();
    seen.extend(drain_to(fence).await);
    assert!(
        seen.iter()
            .all(|path| !path.starts_with(root.join("linked"))),
        "the poller followed the link: {seen:?}"
    );
}

#[tokio::test]
async fn hitting_the_watch_limit_after_registration_demotes_the_library_on_the_next_poll() {
    let (_dir, root) = canonical_tempdir();
    let (_other_dir, other_root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    let other = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();
    watcher.watch_library(other, &other_root).unwrap();

    // What inotify reports when a directory created under a watched root
    // cannot get a watch of its own.
    let full = notify::Error::new(notify::ErrorKind::MaxFilesWatch).add_path(root.join("new"));
    handle_notify_result(Err(full), &watcher.shared);

    let snapshot = status.snapshot();
    assert!(snapshot.limit_reached);
    assert_eq!(
        snapshot.libraries.get(&lib),
        Some(&WatchMode::Native),
        "the callback only queues the demotion"
    );

    assert_eq!(
        watcher.poll_once(),
        vec![lib],
        "the poll reports the library it moved, so the caller can reconcile it"
    );
    let snapshot = status.snapshot();
    assert_eq!(
        snapshot.libraries.get(&lib),
        Some(&WatchMode::Polling(PollReason::WatchLimitReached))
    );
    assert_eq!(
        snapshot.libraries.get(&other),
        Some(&WatchMode::Native),
        "a library the error did not name keeps its native watch"
    );

    // The demoted library is now found by polls, and is not reported as
    // demoted a second time.
    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    assert!(watcher.poll_once().is_empty());
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

#[tokio::test]
async fn a_watch_limit_error_without_a_path_demotes_every_native_library() {
    let (_dir, root) = canonical_tempdir();
    let (_other_dir, other_root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    let other = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();
    watcher.watch_library(other, &other_root).unwrap();

    handle_notify_result(
        Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
        &watcher.shared,
    );
    watcher.poll_once();

    let libraries = status.snapshot().libraries;
    for id in [lib, other] {
        assert_eq!(
            libraries.get(&id),
            Some(&WatchMode::Polling(PollReason::WatchLimitReached))
        );
    }
}

// ── Metrics (issue #457) ────────────────────────────────────────────────
//
// The callback is driven directly, on the test's thread, so the local
// recorder sees what it emits; a real backend calls it from its own.

/// An event is counted against the library that owns its path, by
/// kind; an event under no library, or of a kind that is dropped, is not.
#[tokio::test]
async fn events_are_counted_by_library_and_kind() {
    use crate::test_metrics::MetricsCapture;
    use notify::EventKind;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

    let capture = MetricsCapture::new();
    let _recorder = capture.install();
    let (_dir, root) = canonical_tempdir();
    let (watcher, _status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();

    let event = |kind: EventKind, path: PathBuf| {
        handle_notify_result(Ok(notify::Event::new(kind).add_path(path)), &watcher.shared);
    };
    event(EventKind::Create(CreateKind::File), root.join("a.mkv"));
    event(EventKind::Create(CreateKind::File), root.join("b.mkv"));
    event(EventKind::Modify(ModifyKind::Any), root.join("a.mkv"));
    event(EventKind::Remove(RemoveKind::File), root.join("b.mkv"));
    event(EventKind::Access(AccessKind::Any), root.join("a.mkv"));
    event(
        EventKind::Create(CreateKind::File),
        PathBuf::from("/elsewhere/c.mkv"),
    );

    let recorded = capture.snapshot();
    let library = lib.to_string();
    let count = |kind| {
        recorded.counter(
            "beam_watcher_events_total",
            &[("library", &library), ("kind", kind)],
        )
    };
    assert_eq!(count("created"), Some(2));
    assert_eq!(count("modified"), Some(1));
    assert_eq!(count("removed"), Some(1));
}

/// A backend error is charged to the library whose root holds its path,
/// one naming no path to every library, and a watch that cannot be added
/// to the library being watched.
#[tokio::test]
async fn watcher_errors_are_counted_against_the_libraries_they_touch() {
    use crate::test_metrics::MetricsCapture;

    let capture = MetricsCapture::new();
    let _recorder = capture.install();
    let (_dir, root) = canonical_tempdir();
    let (_other_dir, other_root) = canonical_tempdir();
    let (watcher, _status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    let other = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();
    watcher.watch_library(other, &other_root).unwrap();
    let errors = |id: Uuid| {
        capture
            .snapshot()
            .counter("beam_watcher_errors_total", &[("library", &id.to_string())])
            .unwrap_or(0)
    };

    handle_notify_result(
        Err(notify::Error::generic("read failed").add_path(root.join("a.mkv"))),
        &watcher.shared,
    );
    assert_eq!((errors(lib), errors(other)), (1, 0));

    handle_notify_result(Err(notify::Error::generic("lost")), &watcher.shared);
    assert_eq!((errors(lib), errors(other)), (2, 1));

    handle_notify_result(
        Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch).add_path(other_root.join("d"))),
        &watcher.shared,
    );
    assert_eq!((errors(lib), errors(other)), (2, 2));

    let missing = Uuid::new_v4();
    assert!(
        watcher
            .watch_library(missing, &root.join("not-there"))
            .is_err()
    );
    assert_eq!(errors(missing), 1);
}

#[tokio::test]
async fn unwatching_drops_the_library_from_the_status_and_its_queued_demotion() {
    let (_dir, root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();
    handle_notify_result(
        Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
        &watcher.shared,
    );

    watcher.unwatch_library(lib).unwrap();
    watcher.poll_once();

    assert!(status.snapshot().libraries.is_empty());
}

/// The runtime unwatches a library it may never have registered (one
/// whose watch failed); that is a no-op, and the libraries it does hold
/// keep their watch and keep delivering.
#[tokio::test]
async fn unwatching_a_library_it_never_watched_leaves_the_others_watched() {
    let (_dir, root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Network);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();

    watcher.unwatch_library(Uuid::new_v4()).unwrap();

    assert_eq!(watcher.registered_libraries(), vec![lib]);
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Polling(PollReason::NetworkFilesystem))
    );
    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    watcher.poll_once();
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

/// The runtime re-registers whatever this omits, so it must name exactly
/// the libraries the watcher still holds: not one it unwatched, and not
/// one superseded by a library re-created at the same root.
#[tokio::test]
async fn the_registered_libraries_are_the_ones_still_watched() {
    let (_dir, root) = canonical_tempdir();
    let (_other_dir, other_root) = canonical_tempdir();
    let (_third_dir, third_root) = canonical_tempdir();
    let (watcher, _status) = notify_watcher(FilesystemKind::Local);
    let kept = Uuid::new_v4();
    let unwatched = Uuid::new_v4();
    let superseded = Uuid::new_v4();
    let successor = Uuid::new_v4();
    watcher.watch_library(kept, &root).unwrap();
    watcher.watch_library(unwatched, &other_root).unwrap();
    watcher.watch_library(superseded, &third_root).unwrap();

    watcher.unwatch_library(unwatched).unwrap();
    watcher.watch_library(successor, &third_root).unwrap();

    let mut registered = watcher.registered_libraries();
    registered.sort();
    let mut expected = vec![kept, successor];
    expected.sort();
    assert_eq!(registered, expected);
}

#[tokio::test]
async fn a_library_recreated_at_the_same_root_takes_over_its_events() {
    let (_dir, root) = canonical_tempdir();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let deleted = Uuid::new_v4();
    let recreated = Uuid::new_v4();
    watcher.watch_library(deleted, &root).unwrap();

    // Re-created before the refresh has unwatched the deleted id, then
    // the refresh catches up.
    watcher.watch_library(recreated, &root).unwrap();
    watcher.unwatch_library(deleted).unwrap();

    let libraries = status.snapshot().libraries;
    assert_eq!(libraries.get(&recreated), Some(&WatchMode::Native));
    assert!(!libraries.contains_key(&deleted));

    // The late unwatch did not take the root's watch with it, and the
    // event belongs to the library that exists.
    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(next_event_for(&watcher, &file).await.library_id, recreated);
}

#[test]
fn an_event_under_nested_roots_belongs_to_the_innermost() {
    let outer = Uuid::new_v4();
    let inner = Uuid::new_v4();
    let libraries = [
        WatchedLibrary {
            id: outer,
            root: PathBuf::from("/m"),
            backend: Backend::Native,
            identity: None,
        },
        WatchedLibrary {
            id: inner,
            root: PathBuf::from("/m/movies"),
            backend: Backend::Native,
            identity: None,
        },
    ];
    let cases = [
        ("/m/movies/a.mkv", Some(inner)),
        ("/m/movies2/a.mkv", Some(outer)),
        ("/m/a.mkv", Some(outer)),
        ("/elsewhere/a.mkv", None),
    ];
    for (path, expected) in cases {
        assert_eq!(
            library_owning(&libraries, Path::new(path)),
            expected,
            "{path}"
        );
    }
}

mod properties {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    /// A path of 1 to 4 components from a small alphabet, so generated
    /// roots and paths nest and share prefixes often.
    fn path() -> impl Strategy<Value = PathBuf> {
        prop::collection::vec(prop::sample::select(vec!["m", "mo", "tv", "a"]), 1..=4).prop_map(
            |names| {
                let mut path = PathBuf::from("/");
                path.extend(names);
                path
            },
        )
    }

    fn kind() -> impl Strategy<Value = FsEventKind> {
        prop::sample::select(vec![
            FsEventKind::Created,
            FsEventKind::Modified,
            FsEventKind::Removed,
        ])
    }

    proptest! {
        /// Whatever the order and the overlap of a burst, a drain holds
        /// one event per library and path, of the kind submitted last
        /// for it, and leaves nothing behind.
        #[test]
        fn the_debouncer_keeps_the_last_kind_per_path(
            burst in prop::collection::vec((0u128..2, path(), kind()), 0..40),
        ) {
            let mut debouncer = PathDebouncer::new();
            let mut last: HashMap<(Uuid, PathBuf), FsEventKind> = HashMap::new();
            for (library, path, kind) in &burst {
                let library_id = Uuid::from_u128(*library);
                debouncer.submit(FsEvent { library_id, path: path.clone(), kind: *kind });
                last.insert((library_id, path.clone()), *kind);
            }

            let drained: HashMap<(Uuid, PathBuf), FsEventKind> = debouncer
                .drain()
                .into_iter()
                .map(|FsEvent { library_id, path, kind }| ((library_id, path), kind))
                .collect();
            prop_assert_eq!(drained, last);
            prop_assert!(debouncer.is_empty());
            prop_assert!(debouncer.drain().is_empty());
        }

        /// An event belongs to a library whose root holds it, and to no
        /// library whose root is beneath that one's and also holds it;
        /// to none when no root holds it.
        #[test]
        fn the_innermost_root_holding_a_path_wins(
            roots in prop::collection::btree_set(path(), 0..6),
            event in path(),
        ) {
            let libraries: Vec<WatchedLibrary> = roots
                .iter()
                .enumerate()
                .map(|(n, root)| WatchedLibrary {
                    id: Uuid::from_u128(n as u128),
                    root: root.clone(),
                    backend: Backend::Native,
                    identity: None,
                })
                .collect();
            let holding: Vec<&WatchedLibrary> = libraries
                .iter()
                .filter(|library| event.starts_with(&library.root))
                .collect();

            match library_owning(&libraries, &event) {
                None => prop_assert!(holding.is_empty()),
                Some(owner) => {
                    let owner = libraries.iter().find(|l| l.id == owner).unwrap();
                    prop_assert!(event.starts_with(&owner.root));
                    prop_assert!(
                        !holding
                            .iter()
                            .any(|l| l.root != owner.root && l.root.starts_with(&owner.root)),
                        "a root beneath {:?} also holds {:?}", owner.root, event
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_root_whose_filesystem_cannot_be_probed_is_watched_natively() {
    use crate::services::filesystem_probe::FailingFilesystemProbe;

    let (_dir, root) = canonical_tempdir();
    let status = Arc::new(WatchStatus::new());
    let watcher = NotifyFsWatcher::new(Arc::new(FailingFilesystemProbe), status.clone());
    let lib = Uuid::new_v4();

    assert_eq!(
        watcher.watch_library(lib, &root).unwrap(),
        WatchMode::Native
    );
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Native)
    );
}

#[tokio::test]
async fn without_a_native_watcher_every_library_is_polled() {
    use in_memory::UnavailableNativeWatcherFactory;

    let (_dir, root) = canonical_tempdir();
    let status = Arc::new(WatchStatus::new());
    let watcher = NotifyFsWatcher::with_native_factory(
        Arc::new(FixedFilesystemProbe(FilesystemKind::Local)),
        status.clone(),
        &UnavailableNativeWatcherFactory,
    );
    let lib = Uuid::new_v4();

    let mode = watcher.watch_library(lib, &root).unwrap();
    assert_eq!(mode, WatchMode::Polling(PollReason::NativeUnavailable));
    assert_eq!(status.snapshot().libraries.get(&lib), Some(&mode));
    assert!(!status.snapshot().limit_reached);

    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    watcher.poll_once();
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

#[tokio::test]
async fn hitting_the_watch_limit_while_registering_polls_the_library_instead() {
    use in_memory::WatchLimitNativeWatcherFactory;

    let (_dir, root) = canonical_tempdir();
    let status = Arc::new(WatchStatus::new());
    let factory = WatchLimitNativeWatcherFactory::default();
    let watcher = NotifyFsWatcher::with_native_factory(
        Arc::new(FixedFilesystemProbe(FilesystemKind::Local)),
        status.clone(),
        &factory,
    );
    let lib = Uuid::new_v4();

    let mode = watcher.watch_library(lib, &root).unwrap();
    assert_eq!(mode, WatchMode::Polling(PollReason::WatchLimitReached));
    let snapshot = status.snapshot();
    assert!(snapshot.limit_reached);
    assert_eq!(snapshot.libraries.get(&lib), Some(&mode));
    assert_eq!(
        *factory.unwatched.lock().unwrap(),
        vec![root.clone()],
        "the partial native watch is released"
    );

    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    assert!(
        watcher.poll_once().is_empty(),
        "registered as polled, so no demotion to report"
    );
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

// ── A native watch that stops covering its library (issue #452) ─────────

/// The real backend: removing the root ends its watch, and the poll
/// that follows cannot watch it again, so the library is no longer
/// registered and the status no longer lists it -- the runtime's watch
/// refresh re-registers whatever this omits. Once the root is back, that
/// registration watches it natively again.
#[tokio::test]
async fn a_removed_root_is_deregistered_and_watched_again_once_it_is_back() {
    let (_dir, parent) = canonical_tempdir();
    let root = parent.join("library");
    std::fs::create_dir(&root).unwrap();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();

    std::fs::remove_dir(&root).unwrap();
    // The callback queues the library before it forwards the event.
    next_event_for(&watcher, &root).await;
    assert!(
        watcher.poll_once().is_empty(),
        "nothing was moved to the poller"
    );
    assert!(watcher.registered_libraries().is_empty());
    assert!(!status.snapshot().libraries.contains_key(&lib));

    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        watcher.watch_library(lib, &root).unwrap(),
        WatchMode::Native
    );
    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

/// Removed and re-created before the next poll: the poll watches the
/// new directory at once, rather than leaving the library unwatched
/// until the next refresh.
#[tokio::test]
async fn a_root_recreated_before_the_poll_is_watched_again_by_the_poll() {
    let (_dir, parent) = canonical_tempdir();
    let root = parent.join("library");
    std::fs::create_dir(&root).unwrap();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();
    watcher.watch_library(lib, &root).unwrap();

    std::fs::remove_dir(&root).unwrap();
    next_event_for(&watcher, &root).await;
    std::fs::create_dir(&root).unwrap();
    watcher.poll_once();

    assert_eq!(watcher.registered_libraries(), vec![lib]);
    assert_eq!(
        status.snapshot().libraries.get(&lib),
        Some(&WatchMode::Native)
    );
    let file = root.join("movie.mkv");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(next_event_for(&watcher, &file).await.library_id, lib);
}

fn silent_watcher() -> (NotifyFsWatcher, Arc<WatchStatus>, Arc<Mutex<Vec<PathBuf>>>) {
    use in_memory::SilentNativeWatcherFactory;

    let status = Arc::new(WatchStatus::new());
    let factory = SilentNativeWatcherFactory::default();
    let watcher = NotifyFsWatcher::with_native_factory(
        Arc::new(FixedFilesystemProbe(FilesystemKind::Local)),
        status.clone(),
        &factory,
    );
    (watcher, status, factory.watched)
}

/// An unmount reaches `notify` 8 as nothing at all, so the poll compares
/// what stands at each root with what was watched. A different directory
/// now at the root is watched in its place; a root that is gone leaves
/// the library unregistered and out of the status.
#[tokio::test]
async fn a_root_replaced_without_any_event_is_found_by_the_poll() {
    let (_dir, parent) = canonical_tempdir();
    let replaced = parent.join("replaced");
    let removed = parent.join("removed");
    let untouched = parent.join("untouched");
    for root in [&replaced, &removed, &untouched] {
        std::fs::create_dir(root).unwrap();
    }
    let (watcher, status, watched) = silent_watcher();
    let (replaced_lib, removed_lib, untouched_lib) =
        (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    watcher.watch_library(replaced_lib, &replaced).unwrap();
    watcher.watch_library(removed_lib, &removed).unwrap();
    watcher.watch_library(untouched_lib, &untouched).unwrap();

    // The replacement exists while the original does, so it cannot have
    // been given the original's inode.
    let replacement = parent.join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::remove_dir(&replaced).unwrap();
    std::fs::rename(&replacement, &replaced).unwrap();
    std::fs::remove_dir(&removed).unwrap();

    assert!(watcher.poll_once().is_empty());
    assert_eq!(
        *watched.lock().unwrap(),
        vec![
            replaced.clone(),
            removed.clone(),
            untouched.clone(),
            replaced.clone()
        ],
        "only the replaced root was watched again"
    );
    let mut registered = watcher.registered_libraries();
    registered.sort();
    let mut expected = vec![replaced_lib, untouched_lib];
    expected.sort();
    assert_eq!(registered, expected);
    let libraries = status.snapshot().libraries;
    assert_eq!(libraries.get(&replaced_lib), Some(&WatchMode::Native));
    assert!(!libraries.contains_key(&removed_lib));

    // Watched again, the replaced root is not found replaced again.
    watcher.poll_once();
    assert_eq!(watched.lock().unwrap().len(), 4);
}

/// Which events invalidate a native watch: the root itself removed or
/// renamed, and a rescan notice for every native library it covers.
/// A change beneath a root, an event for another root, and a polled
/// library's root leave the watch alone.
#[tokio::test]
async fn only_events_that_end_a_native_watch_watch_it_again() {
    use notify::event::{Flag, ModifyKind, RemoveKind, RenameMode};
    use notify::{Event, EventKind};

    let (_dir, parent) = canonical_tempdir();
    let (_polled_dir, polled_root) = canonical_tempdir();
    let a = parent.join("a");
    let b = parent.join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();

    let remove_root = Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(a.clone());
    let rename_root =
        Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From))).add_path(a.clone());
    let remove_beneath =
        Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(a.join("season-1"));
    let rename_beneath = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
        .add_path(a.join("season-1"));
    let create_root =
        Event::new(EventKind::Create(notify::event::CreateKind::Folder)).add_path(a.clone());
    let rescan_all = Event::new(EventKind::Other).set_flag(Flag::Rescan);
    let rescan_a = Event::new(EventKind::Other)
        .set_flag(Flag::Rescan)
        .add_path(a.join("season-1"));
    let remove_polled_root =
        Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(polled_root.clone());

    let cases: [(&str, Event, &[&Path]); 8] = [
        ("root removed", remove_root, &[a.as_path()]),
        ("root renamed", rename_root, &[a.as_path()]),
        ("directory beneath removed", remove_beneath, &[]),
        ("directory beneath renamed", rename_beneath, &[]),
        ("root created", create_root, &[]),
        (
            "rescan naming no path",
            rescan_all,
            &[a.as_path(), b.as_path()],
        ),
        ("rescan beneath one root", rescan_a, &[a.as_path()]),
        ("polled root removed", remove_polled_root, &[]),
    ];
    for (case, event, expected) in cases {
        let (watcher, _status, watched) = silent_watcher();
        let (lib_a, lib_b, polled) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        watcher.watch_library(lib_a, &a).unwrap();
        watcher.watch_library(lib_b, &b).unwrap();
        watcher
            .watch_polled(polled, &polled_root, PollReason::NetworkFilesystem)
            .unwrap();
        watched.lock().unwrap().clear();

        handle_notify_result(Ok(event), &watcher.shared);
        watcher.poll_once();

        let mut rewatched = watched.lock().unwrap().clone();
        rewatched.sort();
        assert_eq!(rewatched, expected, "{case}");
        assert_eq!(watcher.registered_libraries().len(), 3, "{case}");
    }
}

/// A library unwatched, or superseded at its root, after its watch was
/// queued to be renewed is not watched again by the poll.
#[tokio::test]
async fn a_renewal_queued_for_a_library_since_unwatched_is_dropped() {
    use notify::event::{Event, EventKind, Flag};

    let (_dir, root) = canonical_tempdir();
    let (_other_dir, other_root) = canonical_tempdir();
    let (watcher, _status, watched) = silent_watcher();
    let (unwatched, superseded, successor) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    watcher.watch_library(unwatched, &root).unwrap();
    watcher.watch_library(superseded, &other_root).unwrap();
    handle_notify_result(
        Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
        &watcher.shared,
    );

    watcher.unwatch_library(unwatched).unwrap();
    watcher.watch_library(successor, &other_root).unwrap();
    watched.lock().unwrap().clear();
    watcher.poll_once();

    assert!(watched.lock().unwrap().is_empty());
    assert_eq!(watcher.registered_libraries(), vec![successor]);
}

#[test]
fn a_library_that_cannot_be_watched_is_reported_and_not_registered() {
    let dir = tempfile::tempdir().unwrap();
    let (watcher, status) = notify_watcher(FilesystemKind::Local);
    let lib = Uuid::new_v4();

    assert!(
        watcher
            .watch_library(lib, &dir.path().join("absent"))
            .is_err()
    );
    assert!(status.snapshot().libraries.is_empty());
}
