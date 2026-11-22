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

impl FsEventKind {
    /// The `kind` label of `beam_watcher_events_total`.
    pub fn label(self) -> &'static str {
        match self {
            FsEventKind::Created => "created",
            FsEventKind::Modified => "modified",
            FsEventKind::Removed => "removed",
        }
    }
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
                    metrics::counter!(
                        "beam_watcher_events_total",
                        "library" => library_id.to_string(),
                        "kind" => kind.label(),
                    )
                    .increment(1);
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
            record_watcher_errors(&affected);
            let mut pending = lock(&shared.pending_demotions);
            for id in affected {
                if !pending.contains(&id) {
                    pending.push(id);
                }
            }
        }
        Err(err) => {
            warn!("filesystem watcher error: {err}");
            // Charged to the libraries whose roots hold the paths it names;
            // one naming no path may be any library's, so it is every one's.
            let affected: Vec<Uuid> = {
                let libraries = lock(&shared.libraries);
                if err.paths.is_empty() {
                    libraries.iter().map(|library| library.id).collect()
                } else {
                    let mut owners: Vec<Uuid> = err
                        .paths
                        .iter()
                        .filter_map(|path| library_owning(&libraries, path))
                        .collect();
                    owners.sort_unstable();
                    owners.dedup();
                    owners
                }
            };
            record_watcher_errors(&affected);
        }
    }
}

/// Count one watcher error against each of `libraries`
/// (`beam_watcher_errors_total{library}`, issue #457).
fn record_watcher_errors(libraries: &[Uuid]) {
    for library_id in libraries {
        metrics::counter!("beam_watcher_errors_total", "library" => library_id.to_string())
            .increment(1);
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
            record_watcher_errors(&[library_id]);
            return Err(WatchError::Backend(
                "no polling watcher is available".to_string(),
            ));
        };
        self.register(library_id, root, Backend::Poll);
        if let Err(e) = lock(poll).watch(root, notify::RecursiveMode::Recursive) {
            self.deregister(library_id);
            record_watcher_errors(&[library_id]);
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
                        record_watcher_errors(&[library_id]);
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
// Out of line for size. Excluded from mutation by path in
// `.cargo/mutants.toml`: an attribute proc macro is unstable on a non-inline
// `mod`, so `#[mutants::skip]` cannot go here.
#[path = "watcher_in_memory.rs"]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory;

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

    /// How many paths have an event pending.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether any events are pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
