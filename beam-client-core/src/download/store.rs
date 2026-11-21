//! Where download records live: the platform's key/value store, one key per
//! server.
//!
//! Per server so that removing a server can take its downloads with it, and
//! through [`KeyValueStore`] rather than a store of its own so that the
//! records live wherever the platform keeps everything else the core
//! persists -- and are readable offline and signed out alike, which is when
//! the downloads screen is used.

use super::{StoredDownload, TransferReport};
use crate::error::StorageError;
use crate::ports::kv::KeyValueStore;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The prefix every server's download key carries: `downloads/{server_id}`.
pub const DOWNLOADS_KEY_PREFIX: &str = "downloads/";

/// Every download record, across every server.
///
/// Every write reads a server's whole list, changes it and writes it back, so
/// the writes take turns: two at once would each write back the list they
/// read, and the second would undo the first -- losing a just-queued download
/// while its items transfer, or bringing back a removed one with a report.
/// The lock is held across the storage calls, which is why it is an async one.
#[derive(Debug, Clone)]
pub struct DownloadStore {
    storage: Arc<dyn KeyValueStore>,
    writes: Arc<Mutex<()>>,
}

impl DownloadStore {
    /// A store over the platform's storage.
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>) -> Self {
        Self {
            storage,
            writes: Arc::new(Mutex::new(())),
        }
    }

    fn key(server_id: &str) -> String {
        format!("{DOWNLOADS_KEY_PREFIX}{server_id}")
    }

    /// One server's downloads, oldest first.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn for_server(&self, server_id: &str) -> Result<Vec<StoredDownload>, StorageError> {
        let Some(raw) = self.storage.get(Self::key(server_id)).await? else {
            return Ok(Vec::new());
        };
        // An unreadable list is read as empty rather than failing every
        // render after it: the screen stays usable, and the next write
        // replaces it. What is lost is the labels on bytes the platform still
        // holds, which removing everything clears.
        Ok(serde_json::from_str(&raw).unwrap_or_else(|error| {
            tracing::warn!(%error, server_id, "discarding an unreadable download list");
            Vec::new()
        }))
    }

    /// Every server's downloads, oldest first.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn all(&self) -> Result<Vec<StoredDownload>, StorageError> {
        let mut all = Vec::new();
        for server_id in self.server_ids().await? {
            all.extend(self.for_server(&server_id).await?);
        }
        sort(&mut all);
        Ok(all)
    }

    /// Add `download`, unless its server already has one holding the file it
    /// names -- the same file, or a part of the same source -- in which case
    /// that one is answered and nothing is written: queueing a title twice
    /// must not transfer it twice.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn insert(&self, download: StoredDownload) -> Result<StoredDownload, StorageError> {
        let _writing = self.writes.lock().await;
        let server_id = download.plan.server_id.clone();
        let mut entries = self.for_server(&server_id).await?;
        let shares_a_part = |existing: &StoredDownload| {
            download.plan.parts.iter().any(|part| {
                existing
                    .plan
                    .parts
                    .iter()
                    .any(|held| held.file_id == part.file_id)
            })
        };
        if let Some(existing) = entries.iter().find(|existing| shares_a_part(existing)) {
            return Ok(existing.clone());
        }
        entries.push(download.clone());
        self.write(&server_id, &entries).await?;
        Ok(download)
    }

    /// Apply each report to the download holding its item, and write only
    /// the servers whose downloads changed. Answers every download, as
    /// [`Self::all`] would after the writes.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn report(
        &self,
        reports: Vec<TransferReport>,
    ) -> Result<Vec<StoredDownload>, StorageError> {
        let _writing = self.writes.lock().await;
        let mut by_server: BTreeMap<String, (Vec<StoredDownload>, bool)> = BTreeMap::new();
        for server_id in self.server_ids().await? {
            let entries = self.for_server(&server_id).await?;
            by_server.insert(server_id, (entries, false));
        }
        for report in reports {
            // A report for an item no download holds -- one removed while its
            // transfer was still being torn down -- changes nothing.
            let holder = by_server.values_mut().find_map(|(entries, changed)| {
                entries
                    .iter_mut()
                    .find(|entry| entry.holds_item(&report.item_id))
                    .map(|entry| (entry, changed))
            });
            if let Some((entry, changed)) = holder
                && entry.apply(report)
            {
                *changed = true;
            }
        }
        let mut all = Vec::new();
        for (server_id, (entries, changed)) in by_server {
            if changed {
                self.write(&server_id, &entries).await?;
            }
            all.extend(entries);
        }
        sort(&mut all);
        Ok(all)
    }

    /// Forget the download of `file_id`, answering it so the platform can
    /// delete the bytes of every item it held.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn remove(&self, file_id: &str) -> Result<Option<StoredDownload>, StorageError> {
        let _writing = self.writes.lock().await;
        for server_id in self.server_ids().await? {
            let mut entries = self.for_server(&server_id).await?;
            if let Some(index) = entries
                .iter()
                .position(|entry| entry.plan.file_id == file_id)
            {
                let removed = entries.remove(index);
                self.write(&server_id, &entries).await?;
                return Ok(Some(removed));
            }
        }
        Ok(None)
    }

    /// Forget every download from `server_id`, answering them. Every other
    /// server's are left alone.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn remove_server(
        &self,
        server_id: &str,
    ) -> Result<Vec<StoredDownload>, StorageError> {
        let _writing = self.writes.lock().await;
        let removed = self.for_server(server_id).await?;
        self.storage.remove(Self::key(server_id)).await?;
        Ok(removed)
    }

    /// Forget every download, answering them.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn remove_all(&self) -> Result<Vec<StoredDownload>, StorageError> {
        let _writing = self.writes.lock().await;
        let removed = self.all().await?;
        for server_id in self.server_ids().await? {
            self.storage.remove(Self::key(&server_id)).await?;
        }
        Ok(removed)
    }

    /// The servers with a download list, in a stable order.
    async fn server_ids(&self) -> Result<Vec<String>, StorageError> {
        let mut ids: Vec<String> = self
            .storage
            .list_keys(DOWNLOADS_KEY_PREFIX.to_owned())
            .await?
            .into_iter()
            .filter_map(|key| key.strip_prefix(DOWNLOADS_KEY_PREFIX).map(str::to_owned))
            .collect();
        ids.sort();
        Ok(ids)
    }

    async fn write(&self, server_id: &str, entries: &[StoredDownload]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return self.storage.remove(Self::key(server_id)).await;
        }
        let raw = serde_json::to_string(entries).map_err(|error| StorageError::Unavailable {
            detail: format!("could not encode the download list: {error}"),
        })?;
        self.storage.put(Self::key(server_id), raw).await
    }
}

/// Oldest first, then by file, so the list does not reorder between renders.
fn sort(downloads: &mut [StoredDownload]) {
    downloads.sort_by(|a, b| {
        a.queued_at_unix
            .cmp(&b.queued_at_unix)
            .then_with(|| a.plan.file_id.cmp(&b.plan.file_id))
    });
}
