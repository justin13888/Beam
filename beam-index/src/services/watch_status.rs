//! What the filesystem watcher is doing for each library, for the admin status.
//!
//! Written by the watcher and the background runtime, read by
//! `GET /v1/admin/status`. Before this existed a library that had silently
//! fallen back to the hourly rescan -- a network mount, an exhausted inotify
//! watch limit -- looked exactly like one being watched.

use std::collections::BTreeMap;
use std::sync::Mutex;

use uuid::Uuid;

/// Why a library is polled rather than natively watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollReason {
    /// The root is on a network filesystem, where native events miss changes
    /// made by other hosts.
    NetworkFilesystem,
    /// Watching it natively hit the OS watch limit
    /// (`fs.inotify.max_user_watches` on Linux).
    WatchLimitReached,
    /// The native watcher could not be created at all.
    NativeUnavailable,
}

/// How one library's changes are observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Native change events (inotify, FSEvents).
    Native,
    /// A tree walk every poll interval.
    Polling(PollReason),
}

/// A point-in-time copy of [`WatchStatus`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchStatusSnapshot {
    /// Whether the watcher runs at all (`BEAM_WATCH_ENABLED`).
    pub enabled: bool,
    /// Whether any native watch has hit the OS watch limit since startup.
    pub limit_reached: bool,
    /// The Linux inotify per-user watch limit, when it could be read.
    pub max_user_watches: Option<u64>,
    /// Every library with a registered watch. A library absent here is not
    /// watched at all -- its registration failed, or the watcher is off.
    pub libraries: BTreeMap<Uuid, WatchMode>,
}

/// Shared, thread-safe watch status. Cheap to update: a mutex around a small
/// map, touched only when a watch is registered, demoted or dropped.
#[derive(Debug, Default)]
pub struct WatchStatus {
    inner: Mutex<WatchStatusSnapshot>,
}

impl WatchStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> WatchStatusSnapshot {
        self.lock().clone()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.lock().enabled = enabled;
    }

    pub fn set_max_user_watches(&self, max_user_watches: Option<u64>) {
        self.lock().max_user_watches = max_user_watches;
    }

    pub fn mark_limit_reached(&self) {
        self.lock().limit_reached = true;
    }

    pub fn set_mode(&self, library_id: Uuid, mode: WatchMode) {
        self.lock().libraries.insert(library_id, mode);
    }

    pub fn remove(&self, library_id: Uuid) {
        self.lock().libraries.remove(&library_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WatchStatusSnapshot> {
        // A panic while holding this lock leaves a map that is still valid:
        // every write above is a single field or map operation.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Where Linux publishes the per-user inotify watch limit.
pub const MAX_USER_WATCHES_PATH: &str = "/proc/sys/fs/inotify/max_user_watches";

/// Parse the contents of [`MAX_USER_WATCHES_PATH`].
pub fn parse_max_user_watches(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

/// Read the per-user inotify watch limit, or `None` where there is none to
/// read (not Linux, or `/proc` unavailable).
pub fn read_max_user_watches() -> Option<u64> {
    std::fs::read_to_string(MAX_USER_WATCHES_PATH)
        .ok()
        .as_deref()
        .and_then(parse_max_user_watches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_user_watches_parses_the_proc_file_format() {
        assert_eq!(parse_max_user_watches("8192\n"), Some(8192));
        assert_eq!(parse_max_user_watches("  524288  "), Some(524_288));
        assert_eq!(parse_max_user_watches(""), None);
        assert_eq!(parse_max_user_watches("-1\n"), None);
        assert_eq!(parse_max_user_watches("lots"), None);
    }

    #[test]
    fn a_removed_library_leaves_the_snapshot() {
        let status = WatchStatus::new();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        status.set_mode(a, WatchMode::Native);
        status.set_mode(b, WatchMode::Polling(PollReason::NetworkFilesystem));
        status.set_mode(a, WatchMode::Polling(PollReason::WatchLimitReached));
        status.remove(b);

        let snapshot = status.snapshot();
        assert_eq!(
            snapshot.libraries.into_iter().collect::<Vec<_>>(),
            vec![(a, WatchMode::Polling(PollReason::WatchLimitReached))],
        );
    }
}
