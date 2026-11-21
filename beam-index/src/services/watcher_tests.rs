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
        },
        WatchedLibrary {
            id: inner,
            root: PathBuf::from("/m/movies"),
            backend: Backend::Native,
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
