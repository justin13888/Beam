//! Wire types of the sign-in and session endpoints (`routes::auth`): the
//! current user, the session list, and the device login's bodies.

use beam_auth::utils::session_store::SessionData;
use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::search::PageInfo;

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: Option<String>,
    pub is_admin: bool,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// Every active session of the current user: what `GET /v1/sessions`
/// returns.
///
/// A connection, as every `/v1` list is, though a user has few enough
/// sessions that it is never paged: `items` is the whole list, both of
/// `page_info`'s `has_*_page` flags are `false`, and it carries no cursors.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct SessionConnection {
    pub items: Vec<SessionSummary>,
    /// Always a single, complete page.
    pub page_info: PageInfo,
}

/// One of the current user's active sessions, as returned by `GET
/// /sessions`. `id` is an opaque row identifier for revocation via `DELETE
/// /sessions/{id}` -- never the session credential itself, which is hashed
/// at rest and cannot be recovered.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct SessionSummary {
    pub id: Uuid,
    pub device_hash: String,
    pub ip: String,
    /// When the session was signed in.
    pub created_at: DateTime<Utc>,
    /// When the session was last used, to the hour: activity slides it
    /// forward at most once an hour.
    pub last_active_at: DateTime<Utc>,
}

impl SessionSummary {
    /// The wire view of one stored session.
    ///
    /// The store keeps whole Unix seconds, which a `DateTime` holds for any
    /// time a session can have; one it cannot is a corrupt row, reported as
    /// such rather than rendered as some other instant.
    pub(crate) fn from_store(id: Uuid, data: SessionData) -> Result<Self, String> {
        let SessionData {
            user_id: _,
            device_hash,
            ip,
            created_at,
            last_active,
        } = data;
        let instant = |secs: i64| {
            DateTime::from_timestamp(secs, 0)
                .ok_or_else(|| format!("session {id} has an unrepresentable timestamp {secs}"))
        };
        Ok(Self {
            id,
            device_hash,
            ip,
            created_at: instant(created_at)?,
            last_active_at: instant(last_active)?,
        })
    }
}

/// What `POST /v1/auth/device` answers: what to show the user, and the handle
/// to poll with.
///
/// The IdP's device code is not here. Beam keeps it and polls the IdP itself,
/// so a client can only ever turn an approval into a Beam session -- never
/// into IdP tokens (ADR-0003's BFF property, kept for native clients).
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginStart {
    /// Opaque, single-flow secret the client polls `POST
    /// /v1/auth/device/token` with. Only its SHA-256 is stored.
    pub device_handle: String,
    /// The code the user enters at `verification_uri`.
    pub user_code: String,
    /// Where the user approves the sign-in, on any device with a browser.
    pub verification_uri: String,
    /// `verification_uri` with the code filled in, when the IdP offers one --
    /// suitable for a QR code.
    pub verification_uri_complete: Option<String>,
    /// How long the user has to approve, in seconds.
    pub expires_in_secs: u64,
    /// The minimum wait between polls, in seconds.
    pub interval_secs: u32,
}

/// The body of `POST /v1/auth/device/token`.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginPoll {
    /// The `device_handle` `POST /v1/auth/device` returned.
    pub device_handle: String,
}

/// Why a device login is still waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLoginWait {
    /// The user has not approved yet. Poll again after `interval_secs`.
    AuthorizationPending,
    /// The poll came too soon. The interval has grown; wait the new
    /// `interval_secs` before polling again.
    SlowDown,
}

/// A device login that has not finished yet.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginPending {
    pub status: DeviceLoginWait,
    /// The minimum wait before the next poll, in seconds.
    pub interval_secs: u32,
}

/// A device login the user approved: the session it minted.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct DeviceLoginComplete {
    /// The `beam_session` credential. Present it as the `beam_session`
    /// cookie on every later request; it is the same opaque session a
    /// browser login mints, with the same idle and absolute expiry.
    pub session_token: String,
    /// The session's hard lifetime from now, in seconds.
    pub session_expires_in_secs: u64,
    /// Who signed in.
    pub user: CurrentUser,
}
