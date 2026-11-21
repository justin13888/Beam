//! The files a scan deferred because they were still being written, handed
//! to whoever retries them (issue #453).
//!
//! A scan that meets a file modified within the settle window leaves it
//! alone, and nothing else would come back to it: no watcher event follows a
//! copy that finished just before boot, and a polled library's snapshot no
//! longer changes. So the scan records each such file here, and the watch
//! consumer ([`crate::runtime`]) takes them into its own retry set, which
//! reconciles each once its delay has passed on the injected clock.
//!
//! Nothing is kept unless a consumer is attached: with the watcher disabled
//! the periodic rescan is the only backstop, and a set nobody drains would
//! only grow.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::Notify;
use uuid::Uuid;

/// A file a scan deferred: retry it after `retry_after`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanDeferral {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub retry_after: Duration,
}

/// The deferred files no consumer has taken yet.
#[derive(Debug, Default)]
pub struct ScanDeferrals {
    state: parking_lot::Mutex<State>,
    /// Woken on every record, so a consumer blocked on its next event takes
    /// the file without waiting for one. One stored permit is enough: a
    /// consumer takes everything recorded at once.
    recorded: Notify,
}

#[derive(Debug, Default)]
struct State {
    /// How many consumers are attached. Recording is a no-op at zero.
    consumers: usize,
    /// Keyed by file, so a file deferred by two scans before it is taken is
    /// retried once, after the later scan's delay.
    pending: HashMap<(Uuid, PathBuf), Duration>,
}

impl ScanDeferrals {
    /// Attach a consumer: from now until the guard drops, deferred files are
    /// kept for [`Self::take`].
    pub fn attach(self: &std::sync::Arc<Self>) -> ScanDeferralsConsumer {
        self.state.lock().consumers += 1;
        ScanDeferralsConsumer {
            deferrals: self.clone(),
        }
    }

    /// Record that a scan deferred `path` of `library_id` by `retry_after`.
    pub fn record(&self, library_id: Uuid, path: PathBuf, retry_after: Duration) {
        {
            let mut state = self.state.lock();
            if state.consumers == 0 {
                return;
            }
            state.pending.insert((library_id, path), retry_after);
        }
        self.recorded.notify_one();
    }
}

/// An attached consumer of [`ScanDeferrals`]. Dropping it detaches it, and
/// the last one to go discards whatever is still pending.
#[derive(Debug)]
pub struct ScanDeferralsConsumer {
    deferrals: std::sync::Arc<ScanDeferrals>,
}

impl ScanDeferralsConsumer {
    /// Take every deferred file recorded since the last take.
    pub fn take(&self) -> Vec<ScanDeferral> {
        let pending = std::mem::take(&mut self.deferrals.state.lock().pending);
        pending
            .into_iter()
            .map(|((library_id, path), retry_after)| ScanDeferral {
                library_id,
                path,
                retry_after,
            })
            .collect()
    }

    /// Resolves once a file has been recorded since this last resolved --
    /// at once, when one was recorded while nobody waited.
    pub async fn recorded(&self) {
        self.deferrals.recorded.notified().await;
    }
}

impl Drop for ScanDeferralsConsumer {
    fn drop(&mut self) {
        let mut state = self.deferrals.state.lock();
        state.consumers -= 1;
        if state.consumers == 0 {
            state.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn deferral(path: &str, secs: u64) -> ScanDeferral {
        ScanDeferral {
            library_id: Uuid::nil(),
            path: PathBuf::from(path),
            retry_after: Duration::from_secs(secs),
        }
    }

    fn record(deferrals: &ScanDeferrals, d: &ScanDeferral) {
        deferrals.record(d.library_id, d.path.clone(), d.retry_after);
    }

    #[test]
    fn nothing_is_kept_while_no_consumer_is_attached() {
        let deferrals = Arc::new(ScanDeferrals::default());
        record(&deferrals, &deferral("/a.mkv", 30));

        let consumer = deferrals.attach();
        assert_eq!(consumer.take(), vec![]);
    }

    #[test]
    fn a_file_is_taken_once_with_its_latest_delay() {
        let deferrals = Arc::new(ScanDeferrals::default());
        let consumer = deferrals.attach();
        record(&deferrals, &deferral("/a.mkv", 30));
        record(&deferrals, &deferral("/a.mkv", 10));

        assert_eq!(consumer.take(), vec![deferral("/a.mkv", 10)]);
        assert_eq!(consumer.take(), vec![], "a take empties the set");
    }

    #[test]
    fn the_last_consumer_to_detach_discards_what_is_pending() {
        let deferrals = Arc::new(ScanDeferrals::default());
        let first = deferrals.attach();
        let second = deferrals.attach();
        record(&deferrals, &deferral("/a.mkv", 30));

        drop(first);
        assert_eq!(second.take(), vec![deferral("/a.mkv", 30)]);
        record(&deferrals, &deferral("/b.mkv", 30));
        drop(second);

        let third = deferrals.attach();
        assert_eq!(third.take(), vec![]);
    }

    #[tokio::test]
    async fn a_record_wakes_a_consumer_that_was_not_yet_waiting() {
        let deferrals = Arc::new(ScanDeferrals::default());
        let consumer = deferrals.attach();
        record(&deferrals, &deferral("/a.mkv", 30));

        // Resolves at once on the permit the record stored; the deadline
        // only bounds a hang.
        tokio::time::timeout(Duration::from_secs(10), consumer.recorded())
            .await
            .expect("the record left a wake-up for the consumer");
    }
}
