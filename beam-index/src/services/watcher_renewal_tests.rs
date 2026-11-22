//! A native watch that stops covering its library (issue #452): a root
//! removed, recreated or replaced beneath a watch, and the poll that
//! notices and watches it again.

use super::*;

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
