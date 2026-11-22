//! What an offline download is: its record, its state and its progress.
//!
//! Android and Apple both keep a list of downloads, and each used to model it
//! on its own -- six states on one, five on the other, progress calculated in
//! Kotlin on one and stored as a fraction on the other. ADR-0012 puts logic
//! above the platform in this crate so that a divergence becomes a bug in one
//! place; this module is that place for downloads.
//!
//! What stays native is the byte transfer itself. Media3's `DownloadManager`
//! writes into the cache ExoPlayer reads from, and Apple's background
//! `URLSession` keeps transferring while the app is suspended; neither can be
//! driven from Rust. So the platform transfers exactly the items a
//! [`DownloadPlan`] names, reports each item's progress back as a
//! [`TransferReport`], and renders the [`DownloadRecord`] the core derives.

mod plan;
mod store;

pub use plan::{DownloadPart, DownloadPlan, DownloadSubtitle, plan_download};
pub use store::{DOWNLOADS_KEY_PREFIX, DownloadStore};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a download has got to.
///
/// The union of what both platforms showed. [`Self::WaitingForNetwork`] is
/// kept apart from [`Self::Paused`] because the remedy differs: a viewer told
/// their download is paused looks for a resume button, which will not help
/// while the only network is one the cellular preference forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    /// Accepted, not yet started.
    Queued,
    /// Bytes are moving.
    Downloading,
    /// Stopped by the viewer, resumable.
    Paused,
    /// Waiting for a network the constraints allow.
    WaitingForNetwork,
    /// Complete and playable offline.
    Completed,
    /// Stopped by an error.
    Failed,
}

/// What the platform's transfer engine says about one item of a plan: a
/// part, by its file id, or a subtitle file, by its sidecar id.
///
/// The platform maps its own engine's states onto [`DownloadState`] -- only
/// it knows that Media3's `STATE_STOPPED` with the user's stop reason is a
/// pause, or that a queued download with unmet requirements is waiting for a
/// network -- and the core decides what the item's report means for the
/// download it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TransferReport {
    /// A [`DownloadPart::file_id`] or a [`DownloadSubtitle::sidecar_id`].
    pub item_id: String,
    /// Where this item has got to.
    pub state: DownloadState,
    /// Bytes of this item fetched so far.
    pub downloaded_bytes: u64,
    /// This item's size, once the server has said; `None` until then.
    pub total_bytes: Option<u64>,
    /// Why this item failed, phrased for a person.
    pub failure_message: Option<String>,
}

/// What a viewer asks to download, with what the downloads screen will need
/// in order to show it offline.
///
/// The display fields are taken now, at enqueue, while a connection that can
/// resolve them still exists: resolving them later would need the network the
/// download exists to do without.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DownloadRequest {
    /// The movie or show the file belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The file the viewer chose.
    pub file_id: String,
    /// Title to show.
    pub title: String,
    /// Second line, such as "S2 E4 - Return".
    pub subtitle: Option<String>,
    /// Where the platform keeps this download's artwork on disk. A local
    /// path, never a remote URL: a URL renders as a placeholder once the
    /// network is gone, which is exactly when the downloads screen is used
    /// (ADR-0015).
    pub artwork_path: Option<String>,
}

/// One download as the downloads screen renders it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DownloadRecord {
    /// The file the viewer chose.
    pub file_id: String,
    /// The movie or show it belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The server it came from, so a removed server can take it along.
    pub server_id: String,
    /// Title to show.
    pub title: String,
    /// Second line, such as "S2 E4 - Return".
    pub subtitle: Option<String>,
    /// The artwork kept on disk for it, if any.
    pub artwork_path: Option<String>,
    /// Where it has got to, across every item of its plan.
    pub state: DownloadState,
    /// Bytes of its parts fetched so far.
    pub downloaded_bytes: u64,
    /// The size of all its parts, or `None` while any part's is unknown.
    pub total_bytes: Option<u64>,
    /// Fraction complete, within `0.0..=1.0`, or `None` while the total is
    /// unknown: a determinate bar that starts at a made-up value is worse
    /// than an indeterminate one.
    pub progress: Option<f32>,
    /// Why it failed, phrased for a person; present only when it has.
    pub failure_message: Option<String>,
    /// Exactly what the platform transfers for it.
    pub plan: DownloadPlan,
}

/// What one item last reported, as it is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ItemTransfer {
    pub(crate) state: DownloadState,
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) failure_message: Option<String>,
}

/// A download as it is persisted: what was asked for, the plan it was given,
/// and what each item last reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredDownload {
    /// The movie or show the file belongs to.
    pub media_id: String,
    /// Title to show.
    pub title: String,
    /// Second line.
    pub subtitle: Option<String>,
    /// The artwork kept on disk for it.
    pub artwork_path: Option<String>,
    /// When it was queued, which orders the list.
    pub queued_at_unix: i64,
    /// What is transferred for it. Holds the file, episode and server ids,
    /// so they are written down once.
    pub plan: DownloadPlan,
    /// What each item last reported, by item id. An item that has not
    /// reported yet is queued.
    #[serde(default)]
    pub(crate) transfers: BTreeMap<String, ItemTransfer>,
}

impl StoredDownload {
    /// A newly queued download: nothing reported yet.
    #[must_use]
    pub fn queued(request: DownloadRequest, plan: DownloadPlan, queued_at_unix: i64) -> Self {
        let DownloadRequest {
            media_id,
            // Both are in the plan, which the core built from this request.
            episode_id: _,
            file_id: _,
            title,
            subtitle,
            artwork_path,
        } = request;
        Self {
            media_id,
            title,
            subtitle,
            artwork_path,
            queued_at_unix,
            plan,
            transfers: BTreeMap::new(),
        }
    }

    /// Whether `item_id` is one of the items its plan transfers.
    #[must_use]
    pub fn holds_item(&self, item_id: &str) -> bool {
        self.plan.item_ids().any(|id| id == item_id)
    }

    /// Record what one of its items reported. Answers whether anything
    /// changed; a report for an item the plan does not name changes nothing.
    pub fn apply(&mut self, report: TransferReport) -> bool {
        let TransferReport {
            item_id,
            state,
            downloaded_bytes,
            total_bytes,
            failure_message,
        } = report;
        if !self.holds_item(&item_id) {
            return false;
        }
        let transfer = ItemTransfer {
            state,
            downloaded_bytes,
            total_bytes,
            // A message belongs to a failure: one carried past a retry would
            // explain a failure that is no longer happening.
            failure_message: failure_message.filter(|_| state == DownloadState::Failed),
        };
        if self.transfers.get(&item_id) == Some(&transfer) {
            return false;
        }
        self.transfers.insert(item_id, transfer);
        true
    }

    /// Where the download as a whole has got to.
    #[must_use]
    pub fn state(&self) -> DownloadState {
        aggregate_state(self.plan.item_ids().map(|id| {
            self.transfers
                .get(id)
                .map_or(DownloadState::Queued, |transfer| transfer.state)
        }))
    }

    /// The record the platform renders.
    #[must_use]
    pub fn to_record(&self) -> DownloadRecord {
        let state = self.state();
        let mut downloaded_bytes = 0_u64;
        let mut total_bytes = Some(0_u64);
        for part in &self.plan.parts {
            let transfer = self.transfers.get(&part.file_id);
            downloaded_bytes = downloaded_bytes
                .saturating_add(transfer.map_or(0, |transfer| transfer.downloaded_bytes));
            // The engine's figure once it has one, the catalog's until then.
            let size = transfer
                .and_then(|transfer| transfer.total_bytes)
                .or(part.size_bytes);
            total_bytes = total_bytes
                .zip(size)
                .map(|(sum, size)| sum.saturating_add(size));
        }
        let failure_message = (state == DownloadState::Failed).then(|| {
            self.plan
                .item_ids()
                .filter_map(|id| self.transfers.get(id))
                .find_map(|transfer| transfer.failure_message.clone())
                .unwrap_or_else(|| "The download stopped. Retry it to carry on.".to_owned())
        });
        DownloadRecord {
            file_id: self.plan.file_id.clone(),
            media_id: self.media_id.clone(),
            episode_id: self.plan.episode_id.clone(),
            server_id: self.plan.server_id.clone(),
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            artwork_path: self.artwork_path.clone(),
            state,
            downloaded_bytes,
            total_bytes,
            progress: if state == DownloadState::Completed {
                Some(1.0)
            } else {
                progress_fraction(downloaded_bytes, total_bytes)
            },
            failure_message,
            plan: self.plan.clone(),
        }
    }
}

/// The state of a download from the states of its items.
///
/// What needs the viewer comes first: a failure, which only a retry clears,
/// then a pause the viewer chose, then a network the constraints refuse. A
/// download is complete only once every item is. One with some items done
/// and the rest queued is downloading: the engine transfers one item at a
/// time, so that mix is the normal state between two items, not a stall.
#[must_use]
pub fn aggregate_state(items: impl IntoIterator<Item = DownloadState>) -> DownloadState {
    let mut seen = [false; 6];
    let mut any = false;
    for state in items {
        any = true;
        seen[state_index(state)] = true;
    }
    let has = |state: DownloadState| seen[state_index(state)];
    if !any {
        return DownloadState::Queued;
    }
    if has(DownloadState::Failed) {
        DownloadState::Failed
    } else if has(DownloadState::Paused) {
        DownloadState::Paused
    } else if has(DownloadState::WaitingForNetwork) {
        DownloadState::WaitingForNetwork
    } else if !has(DownloadState::Queued) && !has(DownloadState::Downloading) {
        DownloadState::Completed
    } else if has(DownloadState::Downloading) || has(DownloadState::Completed) {
        DownloadState::Downloading
    } else {
        DownloadState::Queued
    }
}

const fn state_index(state: DownloadState) -> usize {
    match state {
        DownloadState::Queued => 0,
        DownloadState::Downloading => 1,
        DownloadState::Paused => 2,
        DownloadState::WaitingForNetwork => 3,
        DownloadState::Completed => 4,
        DownloadState::Failed => 5,
    }
}

/// `downloaded` as a fraction of `total`, within `0.0..=1.0`, or `None`
/// while the total is unknown or zero.
///
/// Clamped because an engine can report more bytes than the catalog's size
/// for a file that grew on the server, and a bar past full is a bug a viewer
/// can see.
#[must_use]
#[allow(clippy::cast_precision_loss)] // A fraction for a progress bar.
pub fn progress_fraction(downloaded: u64, total: Option<u64>) -> Option<f32> {
    let total = total.filter(|total| *total > 0)?;
    let fraction = downloaded as f64 / total as f64;
    #[allow(clippy::cast_possible_truncation)] // Within 0..=1 after the clamp.
    Some(fraction.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests;
