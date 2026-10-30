//! Administration: the dashboard, accounts, the operational log, server
//! events, and library management.

use super::{BeamClient, parse_uuid};
use crate::catalog::{
    AdminEvent, AdminLogPage, AdminStatus, AdminUserPage, LibrarySummary, ScanJob,
};
use crate::error::BeamError;

/// The fragment of the problem type `GET /v1/admin/libraries/{id}/scan`
/// answers with when the library has not been scanned since the server
/// started.
const SCAN_NOT_FOUND: &str = "#scan-not-found";

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// The admin dashboard snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn admin_status(&self) -> Result<AdminStatus, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.get_admin_status()).await?;
        Ok(AdminStatus::from_generated(response.into_inner()))
    }

    /// One page of user accounts, oldest first: `first` accounts, after the
    /// page whose `end_cursor` is `after`, or from the start.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn admin_users(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<AdminUserPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::ListAdminUsersParams {
            first: first.map(i64::from),
            after,
        };
        let response = self
            .send(&server_id, client.list_admin_users(params))
            .await?;
        Ok(AdminUserPage::from_generated(
            response.into_inner(),
            &record,
        ))
    }

    /// Block or unblock an account.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `user_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn set_user_disabled(
        &self,
        user_id: String,
        disabled: bool,
    ) -> Result<(), BeamError> {
        let wire_user_id = parse_uuid("user_id", &user_id)?;
        let (server_id, client, _) = self.active_context()?;
        let body = crate::api::types::UpdateAdminUserRequest { disabled };
        self.send(&server_id, client.update_admin_user(wire_user_id, &body))
            .await?;
        Ok(())
    }

    /// One page of the operational log, newest first: `first` lines, after
    /// the page whose `end_cursor` is `after`, or from the start. `total`
    /// counts every line the server holds.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn admin_logs(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<AdminLogPage, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetAdminLogsParams {
            first: first.map(i64::from),
            after,
        };
        let response = self.send(&server_id, client.get_admin_logs(params)).await?;
        Ok(AdminLogPage::from_generated(response.into_inner()))
    }

    /// Recent server events, newest first.
    ///
    /// This polls `GET /v1/admin/events` rather than subscribing to
    /// `/v1/admin/events/stream`. Kynos now describes the streaming endpoint
    /// with OpenAPI 3.2's `itemSchema`, and spargen lowers it to
    /// `Client::stream_admin_events` returning
    /// `support::EventStream<types::AdminEvent>`. Nothing on the UniFFI
    /// surface consumes a stream yet -- UniFFI has no async-iterator type, so
    /// exposing it needs a callback-interface subscription rather than a return
    /// value -- so the feed still polls.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn admin_events(&self, limit: Option<u32>) -> Result<Vec<AdminEvent>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetAdminEventsParams {
            first: limit.map(i64::from),
        };
        let response = self
            .send(&server_id, client.get_admin_events(params))
            .await?;
        // A snapshot, not a paged list: `items` is the most recent `limit`.
        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(AdminEvent::from_generated)
            .collect())
    }

    /// Create a library from a path on the server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<LibrarySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let body = crate::api::types::CreateLibraryRequest { name, root_path };
        let response = self.send(&server_id, client.create_library(&body)).await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// Delete a library and everything indexed into it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `library_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn delete_library(&self, library_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.delete_library(parse_uuid("library_id", &library_id)?),
        )
        .await?;
        Ok(())
    }

    /// Start a rescan of a library. Returns once the server has accepted it:
    /// the scan runs on the server afterwards, and its progress arrives on the
    /// admin event feed.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `library_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator,
    /// [`BeamError::Server`] with status 409 while a scan of the library is
    /// already running, and the [request failures](BeamClient#request-failures).
    pub async fn scan_library(&self, library_id: String) -> Result<(), BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.scan_library(wire_library_id))
            .await?;
        Ok(())
    }

    /// The latest scan of a library since the server started, running or
    /// finished; `None` when it has not been scanned since.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `library_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator,
    /// [`BeamError::NotFound`] for a library the server does not know, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn library_scan(&self, library_id: String) -> Result<Option<ScanJob>, BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        match self
            .send(&server_id, client.get_library_scan(wire_library_id))
            .await
        {
            Ok(response) => Ok(Some(ScanJob::from_generated(response.into_inner()))),
            // The server forgets scans when it restarts, and says so with its
            // own problem type: that is an answer -- not scanned since --
            // rather than a failure, unlike the library itself being unknown.
            Err(BeamError::NotFound { code, .. }) if code.ends_with(SCAN_NOT_FOUND) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Re-fetch metadata for one title.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `media_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn refresh_media_metadata(&self, media_id: String) -> Result<(), BeamError> {
        let wire_media_id = parse_uuid("media_id", &media_id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.refresh_media_metadata(wire_media_id, None),
        )
        .await?;
        Ok(())
    }
}
