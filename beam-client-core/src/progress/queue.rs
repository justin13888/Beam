//! The durable side of playback-progress reporting.
//!
//! Resume points are the one piece of state a viewer notices losing. Watching
//! forty minutes on a train with no signal and finding the title back at zero
//! is a worse failure than the network outage that caused it, so a sample that
//! cannot be sent is persisted and retried rather than dropped.

use crate::clock::Clock;
use crate::error::StorageError;
use crate::ports::kv::KeyValueStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Most entries the queue will hold before evicting the oldest.
///
/// A bound is necessary: an offline device left playing would otherwise grow
/// the queue without limit. 256 distinct files is far more than a viewing
/// session produces, and entries coalesce per file rather than accumulating.
const MAX_ENTRIES: usize = 256;

/// How long an unsent sample stays worth sending.
///
/// A month-old resume point is more likely to be wrong than useful -- the user
/// has almost certainly watched the title elsewhere since.
const MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;

/// Attempts after which a sample is abandoned.
const MAX_ATTEMPTS: u32 = 8;

/// One unsent playback position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedProgress {
    /// Which file.
    pub file_id: String,
    /// Position in seconds.
    pub position_secs: f64,
    /// Total duration, where the player knew it.
    pub duration_secs: Option<f64>,
    /// When the sample was taken, for age-based eviction.
    pub captured_at_unix: i64,
    /// How many send attempts have failed.
    pub attempts: u32,
    /// Earliest time worth retrying, honouring `Retry-After`.
    pub not_before_unix: i64,
}

/// A persisted, de-duplicated queue of unsent progress samples.
#[derive(Debug)]
pub struct ProgressQueue {
    storage: Arc<dyn KeyValueStore>,
    clock: Arc<dyn Clock>,
    server_id: String,
}

impl ProgressQueue {
    /// A queue for one server, backed by the foreign side's storage.
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>, clock: Arc<dyn Clock>, server_id: &str) -> Self {
        Self {
            storage,
            clock,
            server_id: server_id.to_owned(),
        }
    }

    fn key(&self) -> String {
        format!("progress_queue/{}", self.server_id)
    }

    /// Every entry still worth sending, oldest first.
    ///
    /// Expired and exhausted entries are dropped as a side effect of loading,
    /// so a queue that has been offline for a month does not resurrect stale
    /// positions the moment it reconnects.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn load(&self) -> Result<Vec<QueuedProgress>, StorageError> {
        let Some(raw) = self.storage.get(self.key()).await? else {
            return Ok(Vec::new());
        };
        // A corrupted queue is discarded rather than propagated: it holds
        // nothing the user cannot regenerate by pressing play, and failing
        // every future write over it would be worse.
        let entries: Vec<QueuedProgress> = serde_json::from_str(&raw).unwrap_or_default();

        let now = self.clock.now_unix();
        let mut live: Vec<QueuedProgress> = entries
            .into_iter()
            .filter(|entry| {
                now - entry.captured_at_unix < MAX_AGE_SECS && entry.attempts < MAX_ATTEMPTS
            })
            .collect();
        live.sort_by_key(|entry| entry.captured_at_unix);
        Ok(live)
    }

    /// Add or replace the sample for a file.
    ///
    /// Entries are keyed by file, last write wins. Sending two positions for
    /// the same file would at best waste a request and at worst let an older
    /// position overwrite a newer one server-side.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn enqueue(&self, entry: QueuedProgress) -> Result<(), StorageError> {
        let mut entries = self.load().await?;
        entries.retain(|existing| existing.file_id != entry.file_id);
        entries.push(entry);

        // Oldest-first eviction: a newer resume point is the more useful one.
        entries.sort_by_key(|e| e.captured_at_unix);
        while entries.len() > MAX_ENTRIES {
            entries.remove(0);
        }
        self.persist(&entries).await
    }

    /// Forget the sample for a file, after a successful send.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn remove(&self, file_id: &str) -> Result<(), StorageError> {
        let mut entries = self.load().await?;
        entries.retain(|entry| entry.file_id != file_id);
        self.persist(&entries).await
    }

    /// Entries eligible to send now, oldest first.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn ready(&self) -> Result<Vec<QueuedProgress>, StorageError> {
        let now = self.clock.now_unix();
        Ok(self
            .load()
            .await?
            .into_iter()
            .filter(|entry| entry.not_before_unix <= now)
            .collect())
    }

    /// Record a failed attempt, backing the entry off exponentially.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn record_failure(
        &self,
        file_id: &str,
        retry_after_secs: Option<u64>,
    ) -> Result<(), StorageError> {
        let mut entries = self.load().await?;
        let now = self.clock.now_unix();
        for entry in &mut entries {
            if entry.file_id == file_id {
                entry.attempts += 1;
                let backoff = retry_after_secs.map_or_else(
                    // 2s, 4s, 8s, ... capped, so a flapping server is not
                    // hammered but a brief outage still recovers quickly.
                    || 2_i64.saturating_pow(entry.attempts.min(6)),
                    |seconds| i64::try_from(seconds).unwrap_or(i64::MAX),
                );
                // Saturating: the transport clamps `Retry-After`, but this
                // layer must not be the one that panics if a caller does not.
                entry.not_before_unix = now.saturating_add(backoff);
            }
        }
        // An entry that just exhausted its attempts is dropped by `load`, so
        // persisting the incremented count is what retires it.
        entries.retain(|entry| entry.attempts < MAX_ATTEMPTS);
        self.persist(&entries).await
    }

    /// How many entries are held.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn len(&self) -> Result<usize, StorageError> {
        Ok(self.load().await?.len())
    }

    /// Whether the queue holds nothing.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn is_empty(&self) -> Result<bool, StorageError> {
        Ok(self.len().await? == 0)
    }

    async fn persist(&self, entries: &[QueuedProgress]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return self.storage.remove(self.key()).await;
        }
        let encoded =
            serde_json::to_string(entries).map_err(|error| StorageError::Unavailable {
                detail: format!("could not encode the progress queue: {error}"),
            })?;
        self.storage.put(self.key(), encoded).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::ports::kv::InMemoryKeyValueStore;

    fn queue(clock: Arc<TestClock>) -> (ProgressQueue, Arc<InMemoryKeyValueStore>) {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let queue = ProgressQueue::new(storage.clone(), clock, "server-1");
        (queue, storage)
    }

    fn sample(file_id: &str, position: f64, at: i64) -> QueuedProgress {
        QueuedProgress {
            file_id: file_id.to_owned(),
            position_secs: position,
            duration_secs: Some(7200.0),
            captured_at_unix: at,
            attempts: 0,
            not_before_unix: 0,
        }
    }

    #[tokio::test]
    async fn an_enqueued_sample_survives_a_reload() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock);

        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");
        let loaded = queue.load().await.expect("load");
        assert_eq!(loaded.len(), 1);
        assert!((loaded[0].position_secs - 120.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn a_second_sample_for_the_same_file_replaces_the_first() {
        // Sending two positions for one file would at best waste a request and
        // at worst let the older position land last.
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock);

        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");
        queue
            .enqueue(sample("f1", 300.0, 1_010))
            .await
            .expect("enqueue");

        let loaded = queue.load().await.expect("load");
        assert_eq!(loaded.len(), 1);
        assert!((loaded[0].position_secs - 300.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn different_files_coexist() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock);

        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");
        queue
            .enqueue(sample("f2", 60.0, 1_001))
            .await
            .expect("enqueue");
        assert_eq!(queue.len().await.expect("len"), 2);
    }

    #[tokio::test]
    async fn a_successful_send_removes_only_its_own_entry() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock);

        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");
        queue
            .enqueue(sample("f2", 60.0, 1_001))
            .await
            .expect("enqueue");
        queue.remove("f1").await.expect("remove");

        let loaded = queue.load().await.expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].file_id, "f2");
    }

    #[tokio::test]
    async fn entries_older_than_the_retention_window_are_dropped_on_load() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock.clone());

        queue
            .enqueue(sample("stale", 120.0, 1_000))
            .await
            .expect("enqueue");
        clock.advance_secs(MAX_AGE_SECS + 1);

        assert!(queue.is_empty().await.expect("is_empty"));
    }

    #[tokio::test]
    async fn an_entry_is_retired_once_it_exhausts_its_attempts() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock.clone());
        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");

        for _ in 0..MAX_ATTEMPTS {
            queue
                .record_failure("f1", None)
                .await
                .expect("record_failure");
            // Time has to move, or backoff keeps the entry out of `ready`.
            clock.advance_secs(1_000);
        }
        assert!(
            queue.is_empty().await.expect("is_empty"),
            "a permanently failing sample must not occupy the queue forever"
        );
    }

    #[tokio::test]
    async fn backoff_keeps_a_failed_entry_out_of_ready_until_it_elapses() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock.clone());
        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");

        queue
            .record_failure("f1", None)
            .await
            .expect("record_failure");
        assert!(queue.ready().await.expect("ready").is_empty());

        clock.advance_secs(60);
        assert_eq!(queue.ready().await.expect("ready").len(), 1);
    }

    #[tokio::test]
    async fn a_retry_after_hint_is_honoured_over_the_default_backoff() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock.clone());
        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");

        queue
            .record_failure("f1", Some(300))
            .await
            .expect("record_failure");
        clock.advance_secs(299);
        assert!(queue.ready().await.expect("ready").is_empty());
        clock.advance_secs(2);
        assert_eq!(queue.ready().await.expect("ready").len(), 1);
    }

    /// A `Retry-After` at the edge of the integer range neither panics nor
    /// wraps into the past, which would make the entry ready immediately.
    #[tokio::test]
    async fn an_absurd_retry_after_saturates_rather_than_overflowing() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock.clone());
        queue
            .enqueue(sample("f1", 120.0, 1_000))
            .await
            .expect("enqueue");

        queue
            .record_failure("f1", Some(9_223_372_036_854_775_807))
            .await
            .expect("record_failure");

        let entry = queue
            .load()
            .await
            .expect("load")
            .pop()
            .expect("the entry is still held");
        assert!(
            entry.not_before_unix >= clock.now_unix(),
            "next attempt {} is before now {}",
            entry.not_before_unix,
            clock.now_unix()
        );
        assert!(queue.ready().await.expect("ready").is_empty());
    }

    #[tokio::test]
    async fn the_queue_is_bounded_and_evicts_the_oldest_first() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, _storage) = queue(clock);

        for index in 0..(MAX_ENTRIES + 10) {
            let at = 1_000 + i64::try_from(index).expect("small");
            queue
                .enqueue(sample(&format!("f{index}"), 1.0, at))
                .await
                .expect("enqueue");
        }

        let loaded = queue.load().await.expect("load");
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert_eq!(
            loaded[0].file_id, "f10",
            "the ten oldest entries should have been evicted"
        );
    }

    #[tokio::test]
    async fn a_corrupted_queue_is_discarded_rather_than_poisoning_every_write() {
        let clock = Arc::new(TestClock::new(1_000));
        let storage = Arc::new(InMemoryKeyValueStore::new());
        storage
            .put("progress_queue/server-1".to_owned(), "{not json".to_owned())
            .await
            .expect("put");
        let queue = ProgressQueue::new(storage, clock, "server-1");

        assert!(queue.load().await.expect("load").is_empty());
        queue
            .enqueue(sample("f1", 1.0, 1_000))
            .await
            .expect("enqueue");
        assert_eq!(queue.len().await.expect("len"), 1);
    }

    #[tokio::test]
    async fn emptying_the_queue_clears_its_storage_key() {
        let clock = Arc::new(TestClock::new(1_000));
        let (queue, storage) = queue(clock);

        queue
            .enqueue(sample("f1", 1.0, 1_000))
            .await
            .expect("enqueue");
        assert!(storage.has_plain("progress_queue/server-1"));
        queue.remove("f1").await.expect("remove");
        assert!(!storage.has_plain("progress_queue/server-1"));
    }
}
