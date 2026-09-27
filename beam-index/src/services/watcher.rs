//! Filesystem watcher abstraction.
//!
//! Production uses [`NotifyFsWatcher`] (inotify or FSEvents via the `notify`
//! crate, with a polling fallback); tests use [`InMemoryFsWatcher`], whose
//! [`InMemoryFsWatcher::emit`] feeds synthetic events to the consumer with no
//! real filesystem involved.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::services::filesystem_probe::{FilesystemKind, FilesystemProbe};
use crate::services::watch_status::{PollReason, WatchMode, WatchStatus};

/// The kind of filesystem change observed. This is a hint only: reconciliation
/// always re-checks the filesystem, so a mislabelled event still resolves
/// correctly (e.g. a rename surfaces as a Removed + Created pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsEventKind {
    Created,
    Modified,
    Removed,
}

/// A filesystem change within a watched library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEvent {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub kind: FsEventKind,
}

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("failed to watch {0}: {1}")]
    Watch(PathBuf, String),
    #[error("watcher backend error: {0}")]
    Backend(String),
}

/// A source of filesystem-change events for indexed libraries.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait FsWatcher: Send + Sync + std::fmt::Debug {
    /// Recursively watch a library's root directory.
    fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<(), WatchError>;
    /// Stop watching a library. Watching an unknown library is a no-op.
    fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError>;
    /// Rescan every polled library, first moving any natively watched library
    /// that has since hit the watch limit onto the poller. Called by the
    /// runtime once per poll interval; changes it finds arrive through
    /// [`Self::next_event`] like any other. May block on a tree walk.
    fn poll_once(&self);
    /// Await the next event. Returns `None` once the watcher is closed.
    async fn next_event(&self) -> Option<FsEvent>;
}

/// Translate a `notify` event kind into our coarse [`FsEventKind`].
/// Access-only and metadata-only events are dropped (return `None`).
fn translate_event_kind(kind: &notify::EventKind) -> Option<FsEventKind> {
    use notify::EventKind;
    match kind {
        EventKind::Create(_) => Some(FsEventKind::Created),
        EventKind::Modify(_) => Some(FsEventKind::Modified),
        EventKind::Remove(_) => Some(FsEventKind::Removed),
        _ => None,
    }
}

/// How a registered library is being watched by [`NotifyFsWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Native,
    Poll,
}

#[derive(Debug)]
struct WatchedLibrary {
    id: Uuid,
    root: PathBuf,
    backend: Backend,
}

/// State the `notify` callbacks share with [`NotifyFsWatcher`].
///
/// Lock discipline: none of these locks is ever held across a call into a
/// `notify` backend. Both backends invoke the callback from their own thread
/// while a `watch`/`unwatch` call on ours waits for that thread, so holding
/// one of these across such a call is a deadlock.
#[derive(Debug)]
struct Shared {
    /// Watched libraries. Maps an event path back to its library.
    libraries: Mutex<Vec<WatchedLibrary>>,
    /// Natively watched libraries that hit the watch limit after they were
    /// registered, waiting for [`FsWatcher::poll_once`] to move them to the
    /// poller. The callback cannot do it itself: unwatching from inside the
    /// callback waits on the thread that is running the callback.
    pending_demotions: Mutex<Vec<Uuid>>,
    status: Arc<WatchStatus>,
    sender: tokio::sync::mpsc::UnboundedSender<FsEvent>,
}

/// Lock a mutex, recovering the data if a panic poisoned it: every critical
/// section here leaves the data valid.
fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The library whose root contains `path`.
fn library_owning(libraries: &[WatchedLibrary], path: &Path) -> Option<Uuid> {
    libraries
        .iter()
        .find(|library| path.starts_with(&library.root))
        .map(|library| library.id)
}

/// What both `notify` backends do with a result.
///
/// An event is mapped to its library and forwarded. A watch-limit error --
/// inotify reports one when a directory created under a watched root cannot
/// get a watch of its own -- queues the owning library for demotion to the
/// poller, because from then on its native watch silently misses that
/// directory. With no path to attribute the error to, every natively watched
/// library is queued: any of them may be the one missing changes.
fn handle_notify_result(result: notify::Result<notify::Event>, shared: &Shared) {
    match result {
        Ok(event) => {
            let Some(kind) = translate_event_kind(&event.kind) else {
                return;
            };
            for path in event.paths {
                let library_id = library_owning(&lock(&shared.libraries), &path);
                if let Some(library_id) = library_id {
                    let _ = shared.sender.send(FsEvent {
                        library_id,
                        path,
                        kind,
                    });
                }
            }
        }
        Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) => {
            shared.status.mark_limit_reached();
            let affected: Vec<Uuid> = lock(&shared.libraries)
                .iter()
                .filter(|library| library.backend == Backend::Native)
                .filter(|library| {
                    err.paths.is_empty()
                        || err.paths.iter().any(|path| path.starts_with(&library.root))
                })
                .map(|library| library.id)
                .collect();
            warn!(
                paths = ?err.paths,
                libraries = ?affected,
                "filesystem watch limit reached; moving the affected libraries to polling"
            );
            let mut pending = lock(&shared.pending_demotions);
            for id in affected {
                if !pending.contains(&id) {
                    pending.push(id);
                }
            }
        }
        Err(err) => warn!("filesystem watcher error: {err}"),
    }
}

/// Production filesystem watcher: native events where they work, polling
/// where they do not.
///
/// A library is watched natively (inotify on Linux, FSEvents on macOS) unless
/// its root is on a network filesystem -- where changes made by other hosts
/// produce no native event -- or native watching is unavailable or has hit
/// the OS watch limit. Those libraries go to a [`notify::PollWatcher`] in
/// manual mode, which walks them only when [`FsWatcher::poll_once`] is called,
/// so the poll cadence is driven by the runtime's injected clock.
///
/// Neither backend follows symbolic links: Beam's library policy is that a
/// symlink under a root is not part of the library.
pub struct NotifyFsWatcher {
    native: Option<Mutex<notify::RecommendedWatcher>>,
    poll: Option<Mutex<notify::PollWatcher>>,
    probe: Arc<dyn FilesystemProbe>,
    shared: Arc<Shared>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<FsEvent>>,
}

impl std::fmt::Debug for NotifyFsWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyFsWatcher")
            .field("native", &self.native.is_some())
            .field("poll", &self.poll.is_some())
            .field("libraries", &self.shared.libraries)
            .finish_non_exhaustive()
    }
}

impl NotifyFsWatcher {
    /// Build both backends. Never fails: a backend that cannot be created is
    /// logged and left out, and every library then goes to the other one.
    pub fn new(probe: Arc<dyn FilesystemProbe>, status: Arc<WatchStatus>) -> Self {
        use notify::Watcher as _;

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            libraries: Mutex::new(Vec::new()),
            pending_demotions: Mutex::new(Vec::new()),
            status,
            sender,
        });
        let config = notify::Config::default().with_follow_symlinks(false);

        let native = {
            let shared = shared.clone();
            match notify::RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| handle_notify_result(result, &shared),
                config,
            ) {
                Ok(watcher) => Some(Mutex::new(watcher)),
                Err(e) => {
                    error!(
                        "native filesystem watcher unavailable ({e}); every library will be polled"
                    );
                    None
                }
            }
        };
        let poll = {
            let shared = shared.clone();
            match notify::PollWatcher::new(
                move |result: notify::Result<notify::Event>| handle_notify_result(result, &shared),
                config.with_manual_polling(),
            ) {
                Ok(watcher) => Some(Mutex::new(watcher)),
                Err(e) => {
                    error!("polling filesystem watcher unavailable ({e})");
                    None
                }
            }
        };

        Self {
            native,
            poll,
            probe,
            shared,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }

    /// Record a library's backend, replacing any earlier registration.
    fn register(&self, library_id: Uuid, root: &Path, backend: Backend) {
        let mut libraries = lock(&self.shared.libraries);
        libraries.retain(|library| library.id != library_id);
        libraries.push(WatchedLibrary {
            id: library_id,
            root: root.to_path_buf(),
            backend,
        });
    }

    fn deregister(&self, library_id: Uuid) {
        lock(&self.shared.libraries).retain(|library| library.id != library_id);
    }

    /// Hand a library to the poller.
    fn watch_polled(
        &self,
        library_id: Uuid,
        root: &Path,
        reason: PollReason,
    ) -> Result<(), WatchError> {
        use notify::Watcher as _;

        let Some(poll) = &self.poll else {
            self.deregister(library_id);
            return Err(WatchError::Backend(
                "no polling watcher is available".to_string(),
            ));
        };
        self.register(library_id, root, Backend::Poll);
        if let Err(e) = lock(poll).watch(root, notify::RecursiveMode::Recursive) {
            self.deregister(library_id);
            return Err(WatchError::Watch(root.to_path_buf(), e.to_string()));
        }
        self.shared
            .status
            .set_mode(library_id, WatchMode::Polling(reason));
        info!(
            library_id = %library_id,
            root = %root.display(),
            ?reason,
            "polling library for changes"
        );
        Ok(())
    }

    /// Move a natively watched library that hit the watch limit to the poller.
    fn demote(&self, library_id: Uuid) {
        use notify::Watcher as _;

        let root = lock(&self.shared.libraries)
            .iter()
            .find(|library| library.id == library_id && library.backend == Backend::Native)
            .map(|library| library.root.clone());
        let Some(root) = root else {
            // Unwatched, or already demoted, since it was queued.
            return;
        };
        if let Some(native) = &self.native {
            // The partial native watch is dropped so it stops consuming
            // watches another library could use. It may already be gone.
            let _ = lock(native).unwatch(&root);
        }
        if let Err(e) = self.watch_polled(library_id, &root, PollReason::WatchLimitReached) {
            error!(
                library_id = %library_id,
                error = %e,
                "could not poll a library that hit the watch limit"
            );
            self.shared.status.remove(library_id);
        }
    }
}

#[async_trait::async_trait]
impl FsWatcher for NotifyFsWatcher {
    fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<(), WatchError> {
        use notify::Watcher as _;

        let kind = self.probe.kind(root).unwrap_or_else(|e| {
            // Unknown is treated as local: native watching is what Beam did
            // before it could tell, and the periodic rescan still backs it up.
            warn!(
                root = %root.display(),
                error = %e,
                "could not tell what filesystem a library is on; watching it natively"
            );
            FilesystemKind::Local
        });

        let reason = match (kind, &self.native) {
            (FilesystemKind::Network, _) => PollReason::NetworkFilesystem,
            (FilesystemKind::Local, None) => PollReason::NativeUnavailable,
            (FilesystemKind::Local, Some(native)) => {
                // Registered first so events raised while the recursive
                // watch is still being added are attributed.
                self.register(library_id, root, Backend::Native);
                let result = lock(native).watch(root, notify::RecursiveMode::Recursive);
                match result {
                    Ok(()) => {
                        self.shared.status.set_mode(library_id, WatchMode::Native);
                        return Ok(());
                    }
                    Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => {
                        warn!(
                            library_id = %library_id,
                            root = %root.display(),
                            "filesystem watch limit reached; polling this library instead"
                        );
                        self.shared.status.mark_limit_reached();
                        let _ = lock(native).unwatch(root);
                        PollReason::WatchLimitReached
                    }
                    Err(e) => {
                        self.deregister(library_id);
                        return Err(WatchError::Watch(root.to_path_buf(), e.to_string()));
                    }
                }
            }
        };
        self.watch_polled(library_id, root, reason)
    }

    fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError> {
        use notify::Watcher as _;

        let removed = {
            let mut libraries = lock(&self.shared.libraries);
            match libraries
                .iter()
                .position(|library| library.id == library_id)
            {
                Some(pos) => libraries.remove(pos),
                None => return Ok(()),
            }
        };
        lock(&self.shared.pending_demotions).retain(|id| *id != library_id);
        self.shared.status.remove(library_id);

        let result = match (removed.backend, &self.native, &self.poll) {
            (Backend::Native, Some(native), _) => lock(native).unwatch(&removed.root),
            (Backend::Poll, _, Some(poll)) => lock(poll).unwatch(&removed.root),
            _ => Ok(()),
        };
        result.map_err(|e| WatchError::Watch(removed.root, e.to_string()))
    }

    fn poll_once(&self) {
        let demotions = std::mem::take(&mut *lock(&self.shared.pending_demotions));
        for library_id in demotions {
            self.demote(library_id);
        }

        let any_polled = lock(&self.shared.libraries)
            .iter()
            .any(|library| library.backend == Backend::Poll);
        if any_polled
            && let Some(poll) = &self.poll
            && let Err(e) = lock(poll).poll()
        {
            warn!("could not start a filesystem poll: {e}");
        }
    }

    async fn next_event(&self) -> Option<FsEvent> {
        self.receiver.lock().await.recv().await
    }
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// In-memory watcher fake. Events are supplied by tests via [`Self::emit`].
    #[derive(Debug)]
    pub struct InMemoryFsWatcher {
        sender: tokio::sync::mpsc::UnboundedSender<FsEvent>,
        receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<FsEvent>>,
        watched: std::sync::Mutex<Vec<Uuid>>,
        polls: std::sync::atomic::AtomicUsize,
    }

    impl InMemoryFsWatcher {
        pub fn new() -> Self {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            Self {
                sender,
                receiver: tokio::sync::Mutex::new(receiver),
                watched: std::sync::Mutex::new(Vec::new()),
                polls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        /// Push a synthetic event to the consumer.
        pub fn emit(&self, event: FsEvent) {
            let _ = self.sender.send(event);
        }

        /// Library IDs currently registered via `watch_library`, in registration order.
        pub fn watched_libraries(&self) -> Vec<Uuid> {
            self.watched.lock().unwrap().clone()
        }

        /// How many times `poll_once` has been called.
        pub fn poll_count(&self) -> usize {
            self.polls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Default for InMemoryFsWatcher {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl FsWatcher for InMemoryFsWatcher {
        fn watch_library(&self, library_id: Uuid, _root: &Path) -> Result<(), WatchError> {
            self.watched.lock().unwrap().push(library_id);
            Ok(())
        }

        fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError> {
            self.watched.lock().unwrap().retain(|id| *id != library_id);
            Ok(())
        }

        fn poll_once(&self) {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        async fn next_event(&self) -> Option<FsEvent> {
            self.receiver.lock().await.recv().await
        }
    }
}

// Re-exported at the module root so the doubles keep the paths they had before
// they moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryFsWatcher;

/// Coalesces a burst of filesystem events. At most one pending event is kept
/// per `(library_id, path)`; the most recently submitted kind wins, so a
/// trailing `Removed` naturally supersedes an earlier `Modified`.
#[derive(Debug, Default)]
pub struct PathDebouncer {
    pending: std::collections::HashMap<(Uuid, PathBuf), FsEventKind>,
}

impl PathDebouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an event, replacing any earlier pending event for the same path.
    pub fn submit(&mut self, event: FsEvent) {
        self.pending
            .insert((event.library_id, event.path), event.kind);
    }

    /// Take every coalesced event, clearing the buffer.
    pub fn drain(&mut self) -> Vec<FsEvent> {
        self.pending
            .drain()
            .map(|((library_id, path), kind)| FsEvent {
                library_id,
                path,
                kind,
            })
            .collect()
    }

    /// Whether any events are pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(library_id: Uuid, path: &str, kind: FsEventKind) -> FsEvent {
        FsEvent {
            library_id,
            path: PathBuf::from(path),
            kind,
        }
    }

    #[tokio::test]
    async fn test_emit_and_consume() {
        let watcher = InMemoryFsWatcher::new();
        let lib = Uuid::new_v4();
        watcher.emit(event(lib, "/media/a.mp4", FsEventKind::Created));

        let received = watcher.next_event().await.unwrap();
        assert_eq!(received.library_id, lib);
        assert_eq!(received.path, PathBuf::from("/media/a.mp4"));
        assert_eq!(received.kind, FsEventKind::Created);
    }

    #[tokio::test]
    async fn test_events_preserved_in_order_under_backlog() {
        let watcher = InMemoryFsWatcher::new();
        let lib = Uuid::new_v4();
        // A burst is emitted before any consumption.
        watcher.emit(event(lib, "/media/a.mp4", FsEventKind::Created));
        watcher.emit(event(lib, "/media/b.mp4", FsEventKind::Modified));
        watcher.emit(event(lib, "/media/c.mp4", FsEventKind::Removed));

        assert_eq!(
            watcher.next_event().await.unwrap().path,
            PathBuf::from("/media/a.mp4")
        );
        assert_eq!(
            watcher.next_event().await.unwrap().path,
            PathBuf::from("/media/b.mp4")
        );
        assert_eq!(
            watcher.next_event().await.unwrap().path,
            PathBuf::from("/media/c.mp4")
        );
    }

    #[tokio::test]
    async fn test_watch_and_unwatch_tracking() {
        let watcher = InMemoryFsWatcher::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        watcher.watch_library(a, Path::new("/media/a")).unwrap();
        watcher.watch_library(b, Path::new("/media/b")).unwrap();
        assert_eq!(watcher.watched_libraries(), vec![a, b]);

        watcher.unwatch_library(a).unwrap();
        assert_eq!(watcher.watched_libraries(), vec![b]);
        // Unwatching an unknown library is a no-op.
        watcher.unwatch_library(Uuid::new_v4()).unwrap();
        assert_eq!(watcher.watched_libraries(), vec![b]);
    }

    #[test]
    fn test_translate_event_kind() {
        use notify::EventKind;
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

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

        // A change behind the link, then one inside the root as a marker: the
        // poll reports the marker, and nothing under the link before it.
        std::fs::write(outside.join("behind-the-link.mkv"), b"x").unwrap();
        let marker = root.join("marker.mkv");
        std::fs::write(&marker, b"x").unwrap();
        watcher.poll_once();

        let seen = tokio::time::timeout(EVENT_DEADLINE, async {
            let mut seen = Vec::new();
            loop {
                let event = watcher.next_event().await.expect("watcher closed");
                let done = event.path == marker;
                seen.push(event.path);
                if done {
                    return seen;
                }
            }
        })
        .await
        .expect("the poll never reported the marker");
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

        watcher.poll_once();
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

        // The demoted library is now found by polls.
        let file = root.join("movie.mkv");
        std::fs::write(&file, b"x").unwrap();
        watcher.poll_once();
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
}
