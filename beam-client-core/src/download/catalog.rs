//! The persisted record of what has been downloaded.
//!
//! Both platforms' download engines store a URI and an opaque identifier and
//! nothing else -- Media3's download index and `URLSession`'s task descriptions
//! alike. Without a catalogue beside them a downloads screen can show a list of
//! file identifiers and a progress bar, and resolving those over the network is
//! exactly what an offline feature cannot do. So the display fields are written
//! down when the download is queued, while there is still a connection to have
//! learned them from.

use std::sync::Arc;

use crate::error::StorageError;
use crate::ports::kv::KeyValueStore;

use super::record::{DownloadRecord, DownloadReport, DownloadState};

/// The catalogue of downloads for one server.
#[derive(Debug)]
pub struct DownloadCatalog {
    storage: Arc<dyn KeyValueStore>,
    server_id: String,
}

impl DownloadCatalog {
    /// A catalogue for one server, backed by the foreign side's storage.
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>, server_id: &str) -> Self {
        Self {
            storage,
            server_id: server_id.to_owned(),
        }
    }

    fn key(&self) -> String {
        format!("downloads/{}", self.server_id)
    }

    /// Every download, in the order they were queued.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn list(&self) -> Result<Vec<DownloadRecord>, StorageError> {
        let Some(raw) = self.storage.get(self.key()).await? else {
            return Ok(Vec::new());
        };
        // A corrupted catalogue is discarded rather than propagated. It
        // describes files the viewer can queue again, and refusing every
        // subsequent write because one read failed to parse would strand the
        // feature permanently.
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    /// One download, by the file it is for.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn get(&self, file_id: &str) -> Result<Option<DownloadRecord>, StorageError> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .find(|record| record.file_id == file_id))
    }

    /// Record a newly queued download, replacing any existing entry for the
    /// same file.
    ///
    /// Replacing rather than rejecting is deliberate: re-queuing a download
    /// that previously failed is the obvious way for a viewer to retry it, and
    /// the old record carries a stale error message.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn upsert(&self, record: DownloadRecord) -> Result<(), StorageError> {
        let mut records = self.list().await?;
        match records.iter_mut().find(|r| r.file_id == record.file_id) {
            Some(existing) => *existing = record,
            None => records.push(record),
        }
        self.persist(&records).await
    }

    /// Move a download to a new state, leaving its display fields alone.
    ///
    /// Returns the updated record, or `None` if the catalogue has never heard
    /// of the file -- which happens when the platform engine reports progress
    /// for a download the viewer removed a moment earlier, and is not an error.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn set_state(
        &self,
        file_id: &str,
        state: DownloadState,
    ) -> Result<Option<DownloadRecord>, StorageError> {
        let mut records = self.list().await?;
        let Some(record) = records.iter_mut().find(|r| r.file_id == file_id) else {
            return Ok(None);
        };
        record.state = state;
        let updated = record.clone();
        self.persist(&records).await?;
        Ok(Some(updated))
    }

    /// Record transfer progress.
    ///
    /// Also moves the record to [`DownloadState::Downloading`] when bytes
    /// arrive for one that was queued or waiting: the transfer starting *is*
    /// the state change, and the platform engines report it no other way.
    /// A record that has already completed or failed is left alone, so a
    /// late callback cannot resurrect it.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn record_progress(
        &self,
        file_id: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    ) -> Result<Option<DownloadRecord>, StorageError> {
        let mut records = self.list().await?;
        let Some(record) = records.iter_mut().find(|r| r.file_id == file_id) else {
            return Ok(None);
        };

        record.downloaded_bytes = downloaded_bytes;
        // Only ever learn a total, never unlearn one: the engines omit it in
        // some callbacks, and blanking it would flip a determinate progress bar
        // to indeterminate midway through.
        if total_bytes.is_some() {
            record.total_bytes = total_bytes;
        }
        if record.state.is_active() {
            record.state = DownloadState::Downloading;
        }

        let updated = record.clone();
        self.persist(&records).await?;
        Ok(Some(updated))
    }

    /// Apply a platform engine's whole view of its downloads in one pass.
    ///
    /// The engine is treated as authoritative here, unlike
    /// [`Self::record_progress`]: Media3 owns the transfer *and* its own
    /// persistent index, so when it says a download failed or completed, it
    /// did. `record_progress` guards against reviving a finished download
    /// because a straggling `URLSession` callback can arrive after the fact;
    /// an engine sweep carries no such straggler.
    ///
    /// One load and one write regardless of how many downloads are reported,
    /// which is the point -- this runs on every progress tick.
    ///
    /// Reports for files the catalogue does not know are skipped rather than
    /// inserted: without the display fields, a record would render as a bare
    /// identifier, and those are only resolvable while online.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn apply_reports(
        &self,
        reports: &[DownloadReport],
    ) -> Result<Vec<DownloadRecord>, StorageError> {
        let mut records = self.list().await?;
        let mut changed = false;

        for report in reports {
            let Some(record) = records.iter_mut().find(|r| r.file_id == report.file_id) else {
                continue;
            };
            record.downloaded_bytes = report.downloaded_bytes;
            if report.total_bytes.is_some() {
                record.total_bytes = report.total_bytes;
            }
            record.state = report.state.clone();
            changed = true;
        }

        // A sweep that changed nothing must not write: the engines report on a
        // timer even when idle, and rewriting the catalogue every tick would
        // burn flash for no reason.
        if changed {
            self.persist(&records).await?;
        }
        Ok(records)
    }

    /// Forget one download. Removing an unknown file succeeds.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn remove(&self, file_id: &str) -> Result<(), StorageError> {
        let mut records = self.list().await?;
        records.retain(|record| record.file_id != file_id);
        self.persist(&records).await
    }

    /// Forget every download for this server.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn clear(&self) -> Result<(), StorageError> {
        self.storage.remove(self.key()).await
    }

    async fn persist(&self, records: &[DownloadRecord]) -> Result<(), StorageError> {
        // Serializing a `Vec` of plain data cannot fail for any reason a
        // caller could act on, so a failure here is treated as storage being
        // unusable rather than given its own error variant.
        let raw = serde_json::to_string(records).map_err(|error| StorageError::Unavailable {
            detail: format!("could not serialize the download catalogue: {error}"),
        })?;
        self.storage.put(self.key(), raw).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::kv::{FailureMode, InMemoryKeyValueStore};

    fn catalog() -> (DownloadCatalog, Arc<InMemoryKeyValueStore>) {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        (DownloadCatalog::new(storage.clone(), "server-1"), storage)
    }

    fn record(file_id: &str) -> DownloadRecord {
        DownloadRecord {
            file_id: file_id.to_owned(),
            server_id: "server-1".to_owned(),
            media_id: "media-1".to_owned(),
            episode_id: None,
            title: "The Title".to_owned(),
            subtitle: Some("S1 E1".to_owned()),
            poster_url: Some("https://images.example/poster.jpg".to_owned()),
            file_name: Some("show.mkv".to_owned()),
            state: DownloadState::Queued,
            downloaded_bytes: 0,
            total_bytes: None,
        }
    }

    #[tokio::test]
    async fn an_empty_catalogue_lists_nothing_rather_than_failing() {
        let (catalog, _) = catalog();
        assert_eq!(catalog.list().await.unwrap(), Vec::new());
        assert_eq!(catalog.get("absent").await.unwrap(), None);
    }

    /// The display fields are the whole point: they are written down while
    /// there is still a connection, so the screen can render without one.
    #[tokio::test]
    async fn a_queued_download_round_trips_every_display_field() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();

        let stored = catalog.get("file-1").await.unwrap().expect("just stored");
        assert_eq!(stored, record("file-1"));
    }

    /// Re-queuing is how a viewer retries a download that failed, so the second
    /// enqueue must replace the record -- and with it the stale error message.
    #[tokio::test]
    async fn re_queuing_a_file_replaces_its_record_rather_than_duplicating_it() {
        let (catalog, _) = catalog();
        let mut failed = record("file-1");
        failed.state = DownloadState::Failed {
            message: "connection lost".to_owned(),
        };
        catalog.upsert(failed).await.unwrap();
        catalog.upsert(record("file-1")).await.unwrap();

        assert_eq!(catalog.list().await.unwrap().len(), 1);
        assert_eq!(
            catalog.get("file-1").await.unwrap().unwrap().state,
            DownloadState::Queued
        );
    }

    #[tokio::test]
    async fn setting_a_state_leaves_the_display_fields_alone() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();

        let updated = catalog
            .set_state("file-1", DownloadState::Completed)
            .await
            .unwrap()
            .expect("the file is known");

        assert_eq!(updated.state, DownloadState::Completed);
        assert_eq!(updated.title, "The Title");
        assert_eq!(
            updated.poster_url.as_deref(),
            Some("https://images.example/poster.jpg")
        );
    }

    /// The engines report against files the viewer may have removed a moment
    /// earlier. That is ordinary, not an error, and must not resurrect a record.
    #[tokio::test]
    async fn reporting_against_an_unknown_file_is_not_an_error_and_stores_nothing() {
        let (catalog, _) = catalog();

        assert_eq!(
            catalog
                .set_state("gone", DownloadState::Completed)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            catalog.record_progress("gone", 10, Some(20)).await.unwrap(),
            None
        );
        assert_eq!(catalog.list().await.unwrap(), Vec::new());
    }

    /// Bytes arriving *is* how the engines say a transfer started; there is no
    /// separate callback for it.
    #[tokio::test]
    async fn progress_moves_a_queued_download_to_downloading() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();

        let updated = catalog
            .record_progress("file-1", 512, Some(2_048))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.state, DownloadState::Downloading);
        assert_eq!(updated.fraction(), Some(0.25));
    }

    /// A late callback must not undo a terminal state, and a paused download
    /// must not restart itself because a straggling byte count arrived.
    #[tokio::test]
    async fn progress_does_not_revive_a_paused_completed_or_failed_download() {
        for state in [
            DownloadState::Paused,
            DownloadState::Completed,
            DownloadState::Failed {
                message: "gone".to_owned(),
            },
        ] {
            let (catalog, _) = catalog();
            catalog.upsert(record("file-1")).await.unwrap();
            catalog.set_state("file-1", state.clone()).await.unwrap();

            let updated = catalog
                .record_progress("file-1", 512, Some(2_048))
                .await
                .unwrap()
                .unwrap();

            assert_eq!(
                updated.state, state,
                "progress must not move a {state:?} download"
            );
            assert_eq!(
                updated.downloaded_bytes, 512,
                "but the byte count is still recorded"
            );
        }
    }

    /// The engines omit the total in some callbacks. Blanking it would flip a
    /// determinate progress bar to indeterminate midway through.
    #[tokio::test]
    async fn a_total_once_learned_is_never_unlearned() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        catalog
            .record_progress("file-1", 512, Some(2_048))
            .await
            .unwrap();

        let updated = catalog
            .record_progress("file-1", 1_024, None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.total_bytes, Some(2_048));
        assert_eq!(updated.downloaded_bytes, 1_024);
    }

    /// The batch path is what Android uses on every progress tick, and it
    /// treats the engine as authoritative -- including moving a download to a
    /// terminal state the per-file progress path deliberately refuses to.
    #[tokio::test]
    async fn applying_an_engine_sweep_updates_every_reported_download() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        catalog.upsert(record("file-2")).await.unwrap();

        let updated = catalog
            .apply_reports(&[
                DownloadReport {
                    file_id: "file-1".to_owned(),
                    downloaded_bytes: 512,
                    total_bytes: Some(2_048),
                    state: DownloadState::Downloading,
                },
                DownloadReport {
                    file_id: "file-2".to_owned(),
                    downloaded_bytes: 4_096,
                    total_bytes: Some(4_096),
                    state: DownloadState::Completed,
                },
            ])
            .await
            .unwrap();

        assert_eq!(updated.len(), 2);
        assert_eq!(updated[0].fraction(), Some(0.25));
        assert_eq!(updated[1].state, DownloadState::Completed);
    }

    /// A report for a file the catalogue never heard of carries no display
    /// fields, so inserting it would render a bare identifier on a screen that
    /// cannot go and look the title up.
    #[tokio::test]
    async fn an_engine_sweep_skips_downloads_the_catalogue_does_not_know() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();

        let updated = catalog
            .apply_reports(&[DownloadReport {
                file_id: "stranger".to_owned(),
                downloaded_bytes: 1,
                total_bytes: Some(2),
                state: DownloadState::Downloading,
            }])
            .await
            .unwrap();

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].file_id, "file-1");
    }

    /// The engines report on a timer even when nothing is happening. Writing
    /// the catalogue on every one of those ticks would burn flash for nothing.
    #[tokio::test]
    async fn an_engine_sweep_that_changes_nothing_does_not_write() {
        let (catalog, storage) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        storage.set_failure(FailureMode::FailWrites);

        let updated = catalog.apply_reports(&[]).await;

        assert!(
            updated.is_ok(),
            "an empty sweep must not attempt a write, so failing writes cannot break it"
        );
    }

    /// Same rule as the per-file path: the engines omit the total in some
    /// callbacks, and a determinate bar must not flip to indeterminate.
    #[tokio::test]
    async fn an_engine_sweep_never_unlearns_a_total() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        catalog
            .apply_reports(&[DownloadReport {
                file_id: "file-1".to_owned(),
                downloaded_bytes: 512,
                total_bytes: Some(2_048),
                state: DownloadState::Downloading,
            }])
            .await
            .unwrap();

        let updated = catalog
            .apply_reports(&[DownloadReport {
                file_id: "file-1".to_owned(),
                downloaded_bytes: 1_024,
                total_bytes: None,
                state: DownloadState::Downloading,
            }])
            .await
            .unwrap();

        assert_eq!(updated[0].total_bytes, Some(2_048));
    }

    #[tokio::test]
    async fn removing_one_download_leaves_the_others() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        catalog.upsert(record("file-2")).await.unwrap();

        catalog.remove("file-1").await.unwrap();
        catalog.remove("never-existed").await.unwrap();

        let remaining = catalog.list().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].file_id, "file-2");
    }

    #[tokio::test]
    async fn clearing_forgets_every_download() {
        let (catalog, _) = catalog();
        catalog.upsert(record("file-1")).await.unwrap();
        catalog.upsert(record("file-2")).await.unwrap();

        catalog.clear().await.unwrap();

        assert_eq!(catalog.list().await.unwrap(), Vec::new());
    }

    /// Two servers must not see each other's downloads -- removing a server
    /// takes its downloads with it, and it can only do that if they are
    /// separable.
    #[tokio::test]
    async fn catalogues_are_scoped_per_server() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let first = DownloadCatalog::new(storage.clone(), "server-1");
        let second = DownloadCatalog::new(storage.clone(), "server-2");

        first.upsert(record("file-1")).await.unwrap();

        assert_eq!(second.list().await.unwrap(), Vec::new());
        first.clear().await.unwrap();
        second.upsert(record("file-2")).await.unwrap();
        assert_eq!(second.list().await.unwrap().len(), 1);
    }

    /// A locked keystore is exactly the edge case the testing mandate says to
    /// codify rather than verify by hand.
    #[tokio::test]
    async fn storage_failures_are_propagated_rather_than_swallowed() {
        let (catalog, storage) = catalog();
        storage.set_failure(FailureMode::FailAll);

        assert!(catalog.list().await.is_err());
        assert!(catalog.upsert(record("file-1")).await.is_err());
    }

    /// A catalogue that cannot be parsed describes files the viewer can queue
    /// again. Refusing every later write over it would strand the feature.
    #[tokio::test]
    async fn a_corrupted_catalogue_reads_as_empty_and_can_be_written_over() {
        let (catalog, storage) = catalog();
        storage
            .put("downloads/server-1".to_owned(), "{not json".to_owned())
            .await
            .unwrap();

        assert_eq!(catalog.list().await.unwrap(), Vec::new());

        catalog.upsert(record("file-1")).await.unwrap();
        assert_eq!(catalog.list().await.unwrap().len(), 1);
    }
}
