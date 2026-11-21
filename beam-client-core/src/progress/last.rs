//! The last position this device recorded for each file, sent or not.
//!
//! [`super::ProgressQueue`] holds only what has not reached the server yet: a
//! sample the server acknowledged leaves it. So a title watched online and
//! played from a download later, with no network, had nothing on the device to
//! resume from (issue #487). This keeps the newest position reported for each
//! file, whatever became of the send, so an offline Play can start from it.

use crate::clock::Clock;
use crate::error::StorageError;
use crate::ports::kv::KeyValueStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Most files remembered per server before the oldest is forgotten.
///
/// The queue's own bound: a library a device has played from is larger than
/// its unsent backlog, but a resume point for the 257th most recent title is
/// one nobody misses, and the record is rewritten on every report.
const MAX_ENTRIES: usize = 256;

/// One file's last recorded position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedPosition {
    /// Which file.
    pub file_id: String,
    /// Position in seconds.
    pub position_secs: f64,
    /// Total duration, where the player knew it.
    pub duration_secs: Option<f64>,
    /// When the position was recorded, to tell it from an older one.
    pub captured_at_unix: i64,
}

/// A persisted record of the last position reported for each file, for one
/// server.
///
/// Nothing here ages a position out, unlike the queue: a stale queued sample
/// can overwrite the server's newer position, which is why the queue drops it,
/// but a local start position overwrites nothing -- the viewer can seek, and
/// it is used only when the server cannot be asked.
#[derive(Debug)]
pub struct LastPositions {
    storage: Arc<dyn KeyValueStore>,
    clock: Arc<dyn Clock>,
    server_id: String,
}

impl LastPositions {
    /// The record for one server, backed by the foreign side's storage.
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>, clock: Arc<dyn Clock>, server_id: &str) -> Self {
        Self {
            storage,
            clock,
            server_id: server_id.to_owned(),
        }
    }

    /// Where `server_id`'s record is stored, so forgetting the server can
    /// forget it too.
    #[must_use]
    pub fn storage_key(server_id: &str) -> String {
        format!("progress_last/{server_id}")
    }

    async fn load(&self) -> Result<Vec<RecordedPosition>, StorageError> {
        let Some(raw) = self.storage.get(Self::storage_key(&self.server_id)).await? else {
            return Ok(Vec::new());
        };
        // A corrupted record is discarded, as the queue's is: it holds nothing
        // the next report does not rewrite.
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    /// Record `position_secs` as where the viewer is in `file_id`, now.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn record(
        &self,
        file_id: &str,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<(), StorageError> {
        let mut entries = self.load().await?;
        entries.retain(|entry| entry.file_id != file_id);
        entries.push(RecordedPosition {
            file_id: file_id.to_owned(),
            position_secs,
            duration_secs,
            captured_at_unix: self.clock.now_unix(),
        });
        // Oldest first, so eviction forgets the title played longest ago.
        entries.sort_by_key(|entry| entry.captured_at_unix);
        while entries.len() > MAX_ENTRIES {
            entries.remove(0);
        }
        let encoded =
            serde_json::to_string(&entries).map_err(|error| StorageError::Unavailable {
                detail: format!("could not encode the recorded positions: {error}"),
            })?;
        self.storage
            .put(Self::storage_key(&self.server_id), encoded)
            .await
    }

    /// The last position recorded for `file_id`, if any.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn get(&self, file_id: &str) -> Result<Option<RecordedPosition>, StorageError> {
        Ok(self
            .load()
            .await?
            .into_iter()
            .find(|entry| entry.file_id == file_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::ports::kv::InMemoryKeyValueStore;

    fn positions(clock: Arc<TestClock>) -> (LastPositions, Arc<InMemoryKeyValueStore>) {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let positions = LastPositions::new(storage.clone(), clock, "server-1");
        (positions, storage)
    }

    #[tokio::test]
    async fn the_newest_position_for_a_file_wins_and_others_are_kept() {
        let clock = Arc::new(TestClock::new(1_000));
        let (positions, _storage) = positions(Arc::clone(&clock));

        positions
            .record("f1", 120.0, Some(7200.0))
            .await
            .expect("record");
        positions.record("f2", 60.0, None).await.expect("record");
        clock.advance_secs(15);
        positions
            .record("f1", 300.0, Some(7200.0))
            .await
            .expect("record");

        let f1 = positions.get("f1").await.expect("get").expect("recorded");
        assert!((f1.position_secs - 300.0).abs() < f64::EPSILON);
        assert_eq!(f1.captured_at_unix, 1_015);
        let f2 = positions.get("f2").await.expect("get").expect("recorded");
        assert!((f2.position_secs - 60.0).abs() < f64::EPSILON);
        assert_eq!(positions.get("f3").await.expect("get"), None);
    }

    #[tokio::test]
    async fn past_its_bound_the_title_played_longest_ago_is_forgotten() {
        let clock = Arc::new(TestClock::new(1_000));
        let (positions, _storage) = positions(Arc::clone(&clock));

        for file in 0..=MAX_ENTRIES {
            positions
                .record(&format!("f{file}"), 10.0, None)
                .await
                .expect("record");
            clock.advance_secs(1);
        }

        assert_eq!(positions.get("f0").await.expect("get"), None);
        assert!(positions.get("f1").await.expect("get").is_some());
        assert!(
            positions
                .get(&format!("f{MAX_ENTRIES}"))
                .await
                .expect("get")
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_corrupted_record_reads_as_empty_and_is_rewritten() {
        let clock = Arc::new(TestClock::new(1_000));
        let (positions, storage) = positions(clock);
        storage
            .put(
                LastPositions::storage_key("server-1"),
                "not json".to_owned(),
            )
            .await
            .expect("put");

        assert_eq!(positions.get("f1").await.expect("get"), None);
        positions.record("f1", 42.0, None).await.expect("record");
        assert!(positions.get("f1").await.expect("get").is_some());
    }
}
