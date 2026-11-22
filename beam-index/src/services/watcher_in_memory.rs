//! The doubles for [`super::FsWatcher`] and its native backends, out of
//! the watcher's file for size.

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
