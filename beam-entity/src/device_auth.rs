use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// An in-flight RFC 8628 device authorization grant (ADR-0017).
///
/// Created when a native client starts a device login, polled by the client
/// through `beam-server` -- never against the IdP directly -- and deleted when
/// the flow ends: approved, denied, or expired. Keyed by the SHA-256 of the
/// opaque handle the client holds; the handle itself is never stored, so a
/// read of this table cannot be replayed as a poll.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "device_auths")]
pub struct Model {
    /// Lower-case hex SHA-256 of the client's device handle.
    #[sea_orm(primary_key, auto_increment = false)]
    pub handle_hash: String,
    /// The IdP's device code. Server-side only: the client never sees it, so
    /// only Beam can redeem the grant (ADR-0003's BFF property).
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    /// The minimum wait between polls, in seconds. Grows by five on every
    /// `slow_down` (RFC 8628 section 3.5).
    pub interval_secs: i32,
    /// The earliest instant the next poll may reach the IdP.
    pub next_poll_at: DateTimeWithTimeZone,
    pub created_at: DateTimeWithTimeZone,
    pub expires_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
