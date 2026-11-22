//! Offline downloads: the plan, the record, and the reports that move it.
//!
//! A thin shell over [`crate::download`]. The platform transfers the bytes;
//! everything here is what a download *is*, so both clients render the same
//! list from the same records.

use super::playback::PLAYBACK_PLAN_LOOKUP_TIMEOUT;
use super::{BeamClient, parse_uuid};
use crate::download::{
    DownloadPlan, DownloadRecord, DownloadRequest, StoredDownload, TransferReport, plan_download,
};
use crate::error::BeamError;

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// What to transfer to download `file_id` from the active server: every
    /// part of its source and every subtitle file beside one.
    ///
    /// `title_id` is the movie or episode whose sources list the file. As
    /// with [`Self::playback_plan`], a lookup that fails or takes longer than
    /// three seconds -- or no `title_id` at all -- plans the file alone,
    /// which is what a download fetched before the core knew a title's parts.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when a file id is not a UUID, and
    /// [`BeamError::InvalidServerUrl`] when a URL cannot be built on the
    /// server's origin.
    pub async fn download_plan(
        &self,
        title_id: Option<String>,
        episode_id: Option<String>,
        file_id: String,
    ) -> Result<DownloadPlan, BeamError> {
        let server_id = self.require_active()?;
        let record = self.record_for(&server_id)?;
        parse_uuid("file_id", &file_id)?;

        let sources = match title_id.as_deref() {
            Some(title_id) => {
                let lookup = self.media_sources(title_id.to_owned());
                match tokio::time::timeout(PLAYBACK_PLAN_LOOKUP_TIMEOUT, lookup).await {
                    Ok(Ok(sources)) => sources,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "downloading one file alone: its source could not be listed");
                        Vec::new()
                    }
                    Err(_elapsed) => {
                        tracing::warn!("downloading one file alone: listing its source timed out");
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };

        plan_download(
            &server_id,
            title_id.as_deref(),
            episode_id.as_deref(),
            &file_id,
            &sources,
            // Validated first, like every path identifier: interpolated
            // unchecked, an id holding `/` or `..` would name some other
            // resource on the origin and hand it the session cookie.
            |id| {
                let id = parse_uuid("file_id", id)?;
                record.absolute_url(&format!("/v1/files/{id}/download"))
            },
        )
    }

    /// Queue a download on the active server, and answer its record.
    ///
    /// The plan is built now, while the server can be asked, and stored with
    /// the display fields, so the downloads screen renders with no network.
    /// Queueing a file a download on that server already holds answers that
    /// download and queues nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::download_plan`], and [`BeamError::Storage`] when the
    /// record cannot be written.
    pub async fn enqueue_download(
        &self,
        request: DownloadRequest,
    ) -> Result<DownloadRecord, BeamError> {
        // The sources are listed per movie or per episode, never per show.
        let title_id = request
            .episode_id
            .clone()
            .unwrap_or_else(|| request.media_id.clone());
        let plan = self
            .download_plan(
                Some(title_id),
                request.episode_id.clone(),
                request.file_id.clone(),
            )
            .await?;
        let queued = StoredDownload::queued(request, plan, self.clock.now_unix());
        Ok(self.downloads.insert(queued).await?.to_record())
    }

    /// Every download, from every server, oldest first.
    ///
    /// Read from storage alone, so it answers offline and signed out.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the records cannot be read.
    pub async fn downloads(&self) -> Result<Vec<DownloadRecord>, BeamError> {
        Ok(records(self.downloads.all().await?))
    }

    /// Record what the platform's engine reports for some items, and answer
    /// every download as it now stands.
    ///
    /// A report for an item no download holds is ignored, and one that
    /// changes nothing is not written.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the records cannot be read or
    /// written.
    pub async fn report_download_transfers(
        &self,
        reports: Vec<TransferReport>,
    ) -> Result<Vec<DownloadRecord>, BeamError> {
        Ok(records(self.downloads.report(reports).await?))
    }

    /// Forget the download of `file_id`, answering it so the platform can
    /// delete every item its plan names; `None` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the records cannot be written.
    pub async fn remove_download(
        &self,
        file_id: String,
    ) -> Result<Option<DownloadRecord>, BeamError> {
        Ok(self
            .downloads
            .remove(&file_id)
            .await?
            .map(|removed| removed.to_record()))
    }

    /// Forget every download from `server_id`, answering them so the
    /// platform can delete their bytes. Other servers' are left alone.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the records cannot be written.
    pub async fn remove_server_downloads(
        &self,
        server_id: String,
    ) -> Result<Vec<DownloadRecord>, BeamError> {
        Ok(records(self.downloads.remove_server(&server_id).await?))
    }

    /// Forget every download, answering them.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the records cannot be written.
    pub async fn remove_all_downloads(&self) -> Result<Vec<DownloadRecord>, BeamError> {
        Ok(records(self.downloads.remove_all().await?))
    }
}

fn records(stored: Vec<StoredDownload>) -> Vec<DownloadRecord> {
    stored.iter().map(StoredDownload::to_record).collect()
}
