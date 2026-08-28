//! Filesystem watcher abstraction.
//!
//! Production uses [`NotifyFsWatcher`] (inotify via the `notify` crate); tests
//! use [`InMemoryFsWatcher`], whose [`InMemoryFsWatcher::emit`] feeds synthetic
//! events to the consumer with no real filesystem involved.

use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

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

/// Production filesystem watcher backed by the `notify` crate (inotify on Linux).
pub struct NotifyFsWatcher {
    watcher: std::sync::Mutex<notify::RecommendedWatcher>,
    /// Watched libraries as `(id, canonical root)` pairs. Shared with the
    /// `notify` callback so it can map an event path back to its library.
    libraries: std::sync::Arc<std::sync::Mutex<Vec<(Uuid, PathBuf)>>>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<FsEvent>>,
}

impl std::fmt::Debug for NotifyFsWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyFsWatcher")
            .field("libraries", &self.libraries)
            .finish_non_exhaustive()
    }
}

impl NotifyFsWatcher {
    pub fn new() -> Result<Self, WatchError> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let libraries: std::sync::Arc<std::sync::Mutex<Vec<(Uuid, PathBuf)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let libraries_cb = libraries.clone();

        let watcher =
            notify::recommended_watcher(
                move |result: notify::Result<notify::Event>| match result {
                    Ok(event) => {
                        let Some(kind) = translate_event_kind(&event.kind) else {
                            return;
                        };
                        for path in event.paths {
                            let library_id = libraries_cb
                                .lock()
                                .unwrap()
                                .iter()
                                .find(|(_, root)| path.starts_with(root))
                                .map(|(id, _)| *id);
                            if let Some(library_id) = library_id {
                                let _ = sender.send(FsEvent {
                                    library_id,
                                    path,
                                    kind,
                                });
                            }
                        }
                    }
                    Err(e) => tracing::warn!("notify watcher error: {}", e),
                },
            )
            .map_err(|e| WatchError::Backend(e.to_string()))?;

        Ok(Self {
            watcher: std::sync::Mutex::new(watcher),
            libraries,
            receiver: tokio::sync::Mutex::new(receiver),
        })
    }
}

#[async_trait::async_trait]
impl FsWatcher for NotifyFsWatcher {
    fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<(), WatchError> {
        use notify::Watcher as _;

        self.watcher
            .lock()
            .unwrap()
            .watch(root, notify::RecursiveMode::Recursive)
            .map_err(|e| WatchError::Watch(root.to_path_buf(), e.to_string()))?;
        self.libraries
            .lock()
            .unwrap()
            .push((library_id, root.to_path_buf()));
        Ok(())
    }

    fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError> {
        use notify::Watcher as _;

        let root = {
            let mut libraries = self.libraries.lock().unwrap();
            match libraries.iter().position(|(id, _)| *id == library_id) {
                Some(pos) => libraries.remove(pos).1,
                None => return Ok(()),
            }
        };
        self.watcher
            .lock()
            .unwrap()
            .unwatch(&root)
            .map_err(|e| WatchError::Watch(root, e.to_string()))?;
        Ok(())
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
    }

    impl InMemoryFsWatcher {
        pub fn new() -> Self {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            Self {
                sender,
                receiver: tokio::sync::Mutex::new(receiver),
                watched: std::sync::Mutex::new(Vec::new()),
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
}
