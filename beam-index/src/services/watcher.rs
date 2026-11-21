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
/// correctly. A rename, for one, arrives from inotify as a `Modify(Name(..))`
/// event on each side, which [`translate_event_kind`] reads as `Modified`:
/// the reconcile stats the path and finds the old name gone and the new one
/// present, and relinks the file's row to the new name by content (issue
/// #180). An event may name a directory -- one renamed, removed or moved in
/// -- and the reconcile then covers everything beneath it.
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
#[async_trait::async_trait]
pub trait FsWatcher: Send + Sync + std::fmt::Debug {
    /// Recursively watch a library's root directory, returning how it is
    /// watched.
    ///
    /// A polled library's changes are measured against a snapshot taken
    /// here, so a change made before this call is never reported; the caller
    /// reconciles a library that comes back [`WatchMode::Polling`] once. May
    /// block on a tree walk.
    fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<WatchMode, WatchError>;
    /// Stop watching a library. Watching an unknown library is a no-op.
    fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError>;
    /// Rescan every polled library. First, watch again every natively
    /// watched library whose watch no longer covers it -- its root was
    /// removed, renamed, unmounted or remounted, or the backend lost events
    /// -- and move any that has since hit the watch limit onto the poller.
    /// Called by the runtime once per poll interval; changes it finds arrive
    /// through [`Self::next_event`] like any other. May block on a tree walk.
    ///
    /// Returns the libraries moved to the poller by this call. Their native
    /// watch was already missing changes -- that is what the watch limit
    /// means -- and the poller's snapshot, taken now, includes them, so the
    /// caller reconciles each one once.
    fn poll_once(&self) -> Vec<Uuid>;
    /// Every library currently registered, natively or with the poller.
    ///
    /// The watcher drops a registration on its own -- a library it could not
    /// move to the poller, one whose root could not be watched again after
    /// its watch was lost, or one superseded by another at the same root --
    /// so this, not the caller's memory of what it registered, is what the
    /// caller brings in line with the libraries.
    fn registered_libraries(&self) -> Vec<Uuid>;
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

/// The operations [`NotifyFsWatcher`] needs from its native backend.
///
/// A seam only because the two native failures the watcher handles -- no
/// native watcher at all, and the OS watch limit -- cannot be produced on a
/// `TempDir` without exhausting the host's own limit. Production uses
/// [`notify::RecommendedWatcher`] through [`RecommendedNativeWatcherFactory`].
pub trait NativeBackend: Send {
    /// Recursively watch `root`.
    fn watch(&mut self, root: &Path) -> notify::Result<()>;
    fn unwatch(&mut self, root: &Path) -> notify::Result<()>;
}

impl NativeBackend for notify::RecommendedWatcher {
    fn watch(&mut self, root: &Path) -> notify::Result<()> {
        notify::Watcher::watch(self, root, notify::RecursiveMode::Recursive)
    }

    fn unwatch(&mut self, root: &Path) -> notify::Result<()> {
        notify::Watcher::unwatch(self, root)
    }
}

/// Where a native backend's results go.
pub type NotifyHandler = Box<dyn FnMut(notify::Result<notify::Event>) + Send + 'static>;

/// Creates the native backend, or reports that none is available.
pub trait NativeWatcherFactory: Send + Sync + std::fmt::Debug {
    fn create(
        &self,
        handler: NotifyHandler,
        config: notify::Config,
    ) -> notify::Result<Box<dyn NativeBackend>>;
}

/// The production factory: inotify on Linux, FSEvents on macOS.
#[derive(Debug, Default, Clone, Copy)]
pub struct RecommendedNativeWatcherFactory;

impl NativeWatcherFactory for RecommendedNativeWatcherFactory {
    fn create(
        &self,
        handler: NotifyHandler,
        config: notify::Config,
    ) -> notify::Result<Box<dyn NativeBackend>> {
        use notify::Watcher as _;
        Ok(Box::new(notify::RecommendedWatcher::new(handler, config)?))
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
    /// What the root was on disk when its native watch was added; `None` for
    /// a polled library, or where it could not be read.
    identity: Option<RootIdentity>,
}

/// Which directory a root path named when it was watched.
///
/// A native watch follows the directory, not the path: once a filesystem is
/// unmounted from the root, or the root is replaced, the watch is gone or
/// watches something that is no longer the library -- and `notify` 8 reports
/// an unmount (inotify's `IN_UNMOUNT` and `IN_IGNORED`) with no event at all.
/// Comparing this with the root's identity now is how the watcher finds out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn root_identity(root: &Path) -> Option<RootIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(root).ok()?;
    Some(RootIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Without device and inode numbers a replaced root cannot be told apart;
/// only the events that name the root are acted on.
#[cfg(not(unix))]
fn root_identity(_root: &Path) -> Option<RootIdentity> {
    None
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
    /// Natively watched libraries whose watch can no longer be trusted --
    /// their root was removed or renamed, or the backend lost events --
    /// waiting for [`FsWatcher::poll_once`] to watch them again, for the
    /// same reason demotions wait.
    pending_rewatches: Mutex<Vec<Uuid>>,
    status: Arc<WatchStatus>,
    sender: tokio::sync::mpsc::UnboundedSender<FsEvent>,
}

/// Lock a mutex, recovering the data if a panic poisoned it: every critical
/// section here leaves the data valid.
fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The library whose root contains `path`. Registration keeps one entry per
/// root, so two can only both match when their roots nest -- libraries
/// registered before overlaps were refused -- and the innermost wins.
fn library_owning(libraries: &[WatchedLibrary], path: &Path) -> Option<Uuid> {
    libraries
        .iter()
        .filter(|library| path.starts_with(&library.root))
        .max_by_key(|library| library.root.components().count())
        .map(|library| library.id)
}

/// Queue the natively watched libraries an event says the native watch no
/// longer covers, to be watched again (issue #452):
///
/// - the root itself was removed or renamed (inotify's `IN_DELETE_SELF` and
///   `IN_MOVE_SELF`): the watch is gone, or follows the directory away from
///   the root's path;
/// - a rescan notice (inotify's queue overflowed): events were lost, and with
///   them the watches the backend adds for directories created meanwhile, so
///   those subtrees would go unwatched for good.
///
/// A rescan notice naming no path covers every natively watched library.
/// Only native watches are queued: the poller re-reads its whole tree on
/// every poll, so a root it lost is found again when the root comes back.
fn queue_invalidated_watches(event: &notify::Event, shared: &Shared) {
    use notify::EventKind;
    use notify::event::ModifyKind;

    let rescan = event.need_rescan();
    let removes_or_renames = matches!(
        event.kind,
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    );
    if !rescan && !removes_or_renames {
        return;
    }
    let invalidated: Vec<Uuid> = lock(&shared.libraries)
        .iter()
        .filter(|library| library.backend == Backend::Native)
        .filter(|library| {
            let names_this_root = event.paths.contains(&library.root);
            let rescans_this_root = rescan
                && (event.paths.is_empty()
                    || event
                        .paths
                        .iter()
                        .any(|path| path.starts_with(&library.root)));
            (removes_or_renames && names_this_root) || rescans_this_root
        })
        .map(|library| library.id)
        .collect();
    if invalidated.is_empty() {
        return;
    }
    warn!(
        libraries = ?invalidated,
        paths = ?event.paths,
        rescan,
        "a native watch no longer covers its library; it will be watched again"
    );
    let mut pending = lock(&shared.pending_rewatches);
    for id in invalidated {
        if !pending.contains(&id) {
            pending.push(id);
        }
    }
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
            queue_invalidated_watches(&event, shared);
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
/// its root is on a network filesystem, or has one mounted beneath it --
/// where changes made by other hosts produce no native event -- or native
/// watching is unavailable or has hit the OS watch limit. Those libraries go
/// to a [`notify::PollWatcher`] in manual mode, which walks them only when
/// [`FsWatcher::poll_once`] is called, so the poll cadence is driven by the
/// runtime's injected clock.
///
/// Neither backend follows symbolic links: Beam's library policy is that a
/// symlink under a root is not part of the library.
pub struct NotifyFsWatcher {
    native: Option<Mutex<Box<dyn NativeBackend>>>,
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
        Self::with_native_factory(probe, status, &RecommendedNativeWatcherFactory)
    }

    /// [`Self::new`] with the native backend built by `native_factory`.
    pub fn with_native_factory(
        probe: Arc<dyn FilesystemProbe>,
        status: Arc<WatchStatus>,
        native_factory: &dyn NativeWatcherFactory,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            libraries: Mutex::new(Vec::new()),
            pending_demotions: Mutex::new(Vec::new()),
            pending_rewatches: Mutex::new(Vec::new()),
            status,
            sender,
        });
        let config = notify::Config::default().with_follow_symlinks(false);

        let native = {
            let shared = shared.clone();
            match native_factory.create(
                Box::new(move |result: notify::Result<notify::Event>| {
                    handle_notify_result(result, &shared)
                }),
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

    /// Record a library's backend, replacing any earlier registration of the
    /// same library *or of the same root*.
    ///
    /// A root registered under another id belongs to a library that has been
    /// deleted and re-created at the same path: the root is the backend's
    /// key, so the old entry is superseded rather than kept alongside. Its
    /// status entry and any queued demotion go with it, and a later
    /// `unwatch_library` for the old id finds nothing to do -- it must not
    /// unwatch the root the new library now owns.
    fn register(&self, library_id: Uuid, root: &Path, backend: Backend) {
        // Read before the watch is added, so a root replaced while it is
        // being added is found by the next poll rather than recorded as the
        // library.
        let identity = match backend {
            Backend::Native => root_identity(root),
            Backend::Poll => None,
        };
        let superseded: Vec<Uuid> = {
            let mut libraries = lock(&self.shared.libraries);
            let superseded = libraries
                .iter()
                .filter(|library| library.id != library_id && library.root == root)
                .map(|library| library.id)
                .collect();
            libraries.retain(|library| library.id != library_id && library.root != root);
            libraries.push(WatchedLibrary {
                id: library_id,
                root: root.to_path_buf(),
                backend,
                identity,
            });
            superseded
        };
        if !superseded.is_empty() {
            lock(&self.shared.pending_demotions).retain(|id| !superseded.contains(id));
            lock(&self.shared.pending_rewatches).retain(|id| !superseded.contains(id));
            for id in superseded {
                self.shared.status.remove(id);
            }
        }
    }

    fn deregister(&self, library_id: Uuid) {
        lock(&self.shared.libraries).retain(|library| library.id != library_id);
    }

    /// Whether a network filesystem is mounted anywhere beneath a local
    /// `root` (issue #452).
    ///
    /// Native events miss what other hosts change on such a share just as
    /// they would at the root, so the whole library is polled. One poller per
    /// library, rather than a native watch with a polled subtree inside it,
    /// keeps a library on exactly one backend, which demotion, unwatching and
    /// the status all rely on. A probe that fails is treated as finding
    /// nothing, as the root's own probe is.
    ///
    /// Checked once, at registration: a share mounted later is found when the
    /// library is next registered.
    fn network_mount_beneath(&self, library_id: Uuid, root: &Path) -> bool {
        let filesystems = match self.probe.filesystems_beneath(root) {
            Ok(filesystems) => filesystems,
            Err(e) => {
                warn!(
                    root = %root.display(),
                    error = %e,
                    "could not look for filesystems mounted beneath a library; watching it natively"
                );
                return false;
            }
        };
        let network = filesystems
            .into_iter()
            .find(|mount| matches!(self.probe.kind(mount), Ok(FilesystemKind::Network)));
        if let Some(mount) = &network {
            info!(
                library_id = %library_id,
                root = %root.display(),
                mount = %mount.display(),
                "a network filesystem is mounted beneath this library; polling it"
            );
        }
        network.is_some()
    }

    /// Hand a library to the poller.
    fn watch_polled(
        &self,
        library_id: Uuid,
        root: &Path,
        reason: PollReason,
    ) -> Result<WatchMode, WatchError> {
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
        let mode = WatchMode::Polling(reason);
        self.shared.status.set_mode(library_id, mode);
        info!(
            library_id = %library_id,
            root = %root.display(),
            ?reason,
            "polling library for changes"
        );
        Ok(mode)
    }

    /// Move a natively watched library that hit the watch limit to the
    /// poller. Whether it was moved.
    fn demote(&self, library_id: Uuid) -> bool {
        let root = lock(&self.shared.libraries)
            .iter()
            .find(|library| library.id == library_id && library.backend == Backend::Native)
            .map(|library| library.root.clone());
        let Some(root) = root else {
            // Unwatched, or already demoted, since it was queued.
            return false;
        };
        if let Some(native) = &self.native {
            // The partial native watch is dropped so it stops consuming
            // watches another library could use. It may already be gone.
            let _ = lock(native).unwatch(&root);
        }
        match self.watch_polled(library_id, &root, PollReason::WatchLimitReached) {
            Ok(_) => true,
            Err(e) => {
                error!(
                    library_id = %library_id,
                    error = %e,
                    "could not poll a library that hit the watch limit"
                );
                self.shared.status.remove(library_id);
                false
            }
        }
    }

    /// Natively watched libraries whose root no longer names the directory
    /// that was watched: it is gone, or something else -- an unmount, a
    /// remount, a replacement -- now stands at its path. One `stat` per
    /// library, with no lock held across it.
    fn replaced_roots(&self) -> Vec<Uuid> {
        let watched: Vec<(Uuid, PathBuf, RootIdentity)> = lock(&self.shared.libraries)
            .iter()
            .filter(|library| library.backend == Backend::Native)
            .filter_map(|library| Some((library.id, library.root.clone(), library.identity?)))
            .collect();
        watched
            .into_iter()
            .filter(|(_, root, identity)| root_identity(root) != Some(*identity))
            .map(|(library_id, _, _)| library_id)
            .collect()
    }

    /// Drop a natively watched library's watch and register it afresh
    /// (issue #452). Whether it came back polled, so the caller scans it as
    /// it does a demoted one.
    ///
    /// A root that cannot be watched again -- removed, or not mounted yet --
    /// is left unregistered and absent from the status, which is how the
    /// admin status reports a library nothing watches; the runtime's next
    /// watch refresh registers it once its root is back.
    fn rewatch(&self, library_id: Uuid) -> bool {
        let root = {
            let mut libraries = lock(&self.shared.libraries);
            let Some(at) = libraries
                .iter()
                .position(|library| library.id == library_id && library.backend == Backend::Native)
            else {
                // Unwatched, or demoted, since it was queued.
                return false;
            };
            libraries.remove(at).root
        };
        lock(&self.shared.pending_demotions).retain(|id| *id != library_id);
        self.shared.status.remove(library_id);
        if let Some(native) = &self.native {
            // Whatever is left of the old watch. Usually already gone.
            let _ = lock(native).unwatch(&root);
        }
        match self.watch_library(library_id, &root) {
            Ok(mode) => {
                info!(
                    library_id = %library_id,
                    root = %root.display(),
                    ?mode,
                    "watching a library again after its watch was lost"
                );
                matches!(mode, WatchMode::Polling(_))
            }
            Err(e) => {
                warn!(
                    library_id = %library_id,
                    error = %e,
                    "a library's watch was lost and its root cannot be watched again; \
                     it is not watched until a watch refresh finds its root"
                );
                false
            }
        }
    }
}

#[async_trait::async_trait]
impl FsWatcher for NotifyFsWatcher {
    fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<WatchMode, WatchError> {
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
            (FilesystemKind::Local, Some(_)) if self.network_mount_beneath(library_id, root) => {
                PollReason::NetworkFilesystem
            }
            (FilesystemKind::Local, Some(native)) => {
                // Registered first so events raised while the recursive
                // watch is still being added are attributed.
                self.register(library_id, root, Backend::Native);
                let result = lock(native).watch(root);
                match result {
                    Ok(()) => {
                        self.shared.status.set_mode(library_id, WatchMode::Native);
                        return Ok(WatchMode::Native);
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
        lock(&self.shared.pending_rewatches).retain(|id| *id != library_id);
        self.shared.status.remove(library_id);

        let result = match (removed.backend, &self.native, &self.poll) {
            (Backend::Native, Some(native), _) => lock(native).unwatch(&removed.root),
            (Backend::Poll, _, Some(poll)) => lock(poll).unwatch(&removed.root),
            _ => Ok(()),
        };
        result.map_err(|e| WatchError::Watch(removed.root, e.to_string()))
    }

    fn poll_once(&self) -> Vec<Uuid> {
        let mut rewatches = std::mem::take(&mut *lock(&self.shared.pending_rewatches));
        for library_id in self.replaced_roots() {
            if !rewatches.contains(&library_id) {
                rewatches.push(library_id);
            }
        }
        let mut moved_to_poller: Vec<Uuid> = rewatches
            .into_iter()
            .filter(|library_id| self.rewatch(*library_id))
            .collect();

        let demotions = std::mem::take(&mut *lock(&self.shared.pending_demotions));
        for library_id in demotions {
            if self.demote(library_id) && !moved_to_poller.contains(&library_id) {
                moved_to_poller.push(library_id);
            }
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
        moved_to_poller
    }

    fn registered_libraries(&self) -> Vec<Uuid> {
        lock(&self.shared.libraries)
            .iter()
            .map(|library| library.id)
            .collect()
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
        /// What `watch_library` reports per library; `Native` when absent.
        modes: std::sync::Mutex<std::collections::HashMap<Uuid, WatchMode>>,
        /// What the next `poll_once` reports as demoted.
        demotions: std::sync::Mutex<Vec<Uuid>>,
        /// What the next `poll_once` deregisters without reporting, as a
        /// demotion that fails does.
        failed_demotions: std::sync::Mutex<Vec<Uuid>>,
        /// Libraries whose next `watch_library` fails, leaving them
        /// unregistered.
        failed_watches: std::sync::Mutex<Vec<Uuid>>,
        /// Every library `watch_library` was asked for, in order.
        attempts: std::sync::Mutex<Vec<Uuid>>,
        /// Bumped whenever the registrations change, so a test can wait for
        /// one made on another task.
        changes: tokio::sync::watch::Sender<u64>,
    }

    impl InMemoryFsWatcher {
        pub fn new() -> Self {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            Self {
                sender,
                receiver: tokio::sync::Mutex::new(receiver),
                watched: std::sync::Mutex::new(Vec::new()),
                polls: std::sync::atomic::AtomicUsize::new(0),
                modes: std::sync::Mutex::new(std::collections::HashMap::new()),
                demotions: std::sync::Mutex::new(Vec::new()),
                failed_demotions: std::sync::Mutex::new(Vec::new()),
                failed_watches: std::sync::Mutex::new(Vec::new()),
                attempts: std::sync::Mutex::new(Vec::new()),
                changes: tokio::sync::watch::channel(0).0,
            }
        }

        /// Wait until `library_id` is registered, however it gets there.
        pub async fn until_watched(&self, library_id: Uuid) {
            let mut changes = self.changes.subscribe();
            changes
                .wait_for(|_| self.watched_libraries().contains(&library_id))
                .await
                .expect("the watcher outlives its subscribers");
        }

        /// Make `watch_library` report `mode` for `library_id`.
        pub fn register_as(&self, library_id: Uuid, mode: WatchMode) {
            self.modes.lock().unwrap().insert(library_id, mode);
        }

        /// Make the next `poll_once` report `library_id` as moved to polling.
        pub fn demote_on_next_poll(&self, library_id: Uuid) {
            self.demotions.lock().unwrap().push(library_id);
        }

        /// Make the next `poll_once` fail to demote `library_id`: it is
        /// deregistered and not reported.
        pub fn fail_demotion_on_next_poll(&self, library_id: Uuid) {
            self.failed_demotions.lock().unwrap().push(library_id);
        }

        /// Make the next `watch_library` of `library_id` fail.
        pub fn fail_next_watch(&self, library_id: Uuid) {
            self.failed_watches.lock().unwrap().push(library_id);
        }

        /// How many times `watch_library` has been asked for `library_id`,
        /// failed or not.
        pub fn watch_attempts(&self, library_id: Uuid) -> usize {
            self.attempts
                .lock()
                .unwrap()
                .iter()
                .filter(|id| **id == library_id)
                .count()
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
        fn watch_library(&self, library_id: Uuid, root: &Path) -> Result<WatchMode, WatchError> {
            self.attempts.lock().unwrap().push(library_id);
            {
                let mut failed = self.failed_watches.lock().unwrap();
                if let Some(at) = failed.iter().position(|id| *id == library_id) {
                    failed.remove(at);
                    self.changes.send_modify(|generation| *generation += 1);
                    return Err(WatchError::Watch(
                        root.to_path_buf(),
                        "refused by the test".to_string(),
                    ));
                }
            }
            self.watched.lock().unwrap().push(library_id);
            self.changes.send_modify(|generation| *generation += 1);
            Ok(self
                .modes
                .lock()
                .unwrap()
                .get(&library_id)
                .copied()
                .unwrap_or(WatchMode::Native))
        }

        fn unwatch_library(&self, library_id: Uuid) -> Result<(), WatchError> {
            self.watched.lock().unwrap().retain(|id| *id != library_id);
            self.changes.send_modify(|generation| *generation += 1);
            Ok(())
        }

        fn poll_once(&self) -> Vec<Uuid> {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let failed = std::mem::take(&mut *self.failed_demotions.lock().unwrap());
            self.watched
                .lock()
                .unwrap()
                .retain(|id| !failed.contains(id));
            std::mem::take(&mut *self.demotions.lock().unwrap())
        }

        fn registered_libraries(&self) -> Vec<Uuid> {
            self.watched_libraries()
        }

        async fn next_event(&self) -> Option<FsEvent> {
            self.receiver.lock().await.recv().await
        }
    }

    /// A host with no native watcher: creating one fails.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct UnavailableNativeWatcherFactory;

    impl NativeWatcherFactory for UnavailableNativeWatcherFactory {
        fn create(
            &self,
            _handler: NotifyHandler,
            _config: notify::Config,
        ) -> notify::Result<Box<dyn NativeBackend>> {
            Err(notify::Error::generic("no native watcher on this host"))
        }
    }

    /// A native backend already at the OS watch limit: every `watch` fails
    /// with `MaxFilesWatch`. Records what it was asked to unwatch.
    #[derive(Debug, Default, Clone)]
    pub struct WatchLimitNativeWatcherFactory {
        pub unwatched: Arc<Mutex<Vec<PathBuf>>>,
    }

    struct WatchLimitBackend {
        unwatched: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl NativeBackend for WatchLimitBackend {
        fn watch(&mut self, root: &Path) -> notify::Result<()> {
            Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch).add_path(root.to_path_buf()))
        }

        fn unwatch(&mut self, root: &Path) -> notify::Result<()> {
            self.unwatched.lock().unwrap().push(root.to_path_buf());
            Ok(())
        }
    }

    impl NativeWatcherFactory for WatchLimitNativeWatcherFactory {
        fn create(
            &self,
            _handler: NotifyHandler,
            _config: notify::Config,
        ) -> notify::Result<Box<dyn NativeBackend>> {
            Ok(Box::new(WatchLimitBackend {
                unwatched: self.unwatched.clone(),
            }))
        }
    }

    /// A native backend whose watches succeed and never report anything --
    /// what `notify` 8 does when the filesystem under a watched root is
    /// unmounted. A root that does not exist fails, as it does for inotify.
    /// Records every root it watched.
    #[derive(Debug, Default, Clone)]
    pub struct SilentNativeWatcherFactory {
        pub watched: Arc<Mutex<Vec<PathBuf>>>,
    }

    struct SilentBackend {
        watched: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl NativeBackend for SilentBackend {
        fn watch(&mut self, root: &Path) -> notify::Result<()> {
            if !root.is_dir() {
                return Err(notify::Error::path_not_found().add_path(root.to_path_buf()));
            }
            self.watched.lock().unwrap().push(root.to_path_buf());
            Ok(())
        }

        fn unwatch(&mut self, _root: &Path) -> notify::Result<()> {
            Ok(())
        }
    }

    impl NativeWatcherFactory for SilentNativeWatcherFactory {
        fn create(
            &self,
            _handler: NotifyHandler,
            _config: notify::Config,
        ) -> notify::Result<Box<dyn NativeBackend>> {
            Ok(Box::new(SilentBackend {
                watched: self.watched.clone(),
            }))
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

    fn mounted_watcher(
        mounts: Vec<(PathBuf, FilesystemKind)>,
    ) -> (NotifyFsWatcher, Arc<WatchStatus>) {
        use crate::services::filesystem_probe::MountedFilesystemProbe;

        let status = Arc::new(WatchStatus::new());
        let watcher =
            NotifyFsWatcher::new(Arc::new(MountedFilesystemProbe { mounts }), status.clone());
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
}
