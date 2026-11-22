//! The device authorization grant's poll loop (RFC 8628 section 3.5),
//! written once for every client.

use super::BeamClient;
use crate::error::BeamError;
use crate::session::{DeviceLoginPrompt, DeviceLoginStep, UserSummary};

/// What RFC 8628 section 3.5 has a client add to its interval on `slow_down`.
const SLOW_DOWN_STEP_SECS: u64 = 5;

/// The problem type the server answers an expired flow with, which a flow
/// the core finds expired by its own clock ends with too.
const DEVICE_LOGIN_EXPIRED: &str =
    "https://beam.justinchung.net/reference/errors/#device-login-expired";

/// What one poll's outcome means for the loop.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Poll again after `wait_secs`, at `interval_secs` from then on.
    PollAfter { wait_secs: u64, interval_secs: u64 },
    /// Signed in.
    Done(UserSummary),
    /// The flow is over.
    Fail(BeamError),
}

/// The pacing rule, kept apart from the I/O so every branch is a table row.
///
/// `slow_down` grows the interval by at least five seconds, whatever the
/// server reported, and a pending answer never shrinks it: either would have
/// the next poll come too soon. A rate limit waits as long as `Retry-After`
/// asks if that is longer than the interval. A failure a later poll could get
/// past -- a retryable server error, a lost connection -- keeps the flow:
/// the viewer may be halfway through approving it on their phone. Anything
/// else ends it.
fn next(interval_secs: u64, polled: Result<DeviceLoginStep, BeamError>) -> Next {
    let keep = |interval_secs: u64| Next::PollAfter {
        wait_secs: interval_secs,
        interval_secs,
    };
    match polled {
        Ok(DeviceLoginStep::SignedIn { user }) => Next::Done(user),
        Ok(DeviceLoginStep::Waiting {
            interval_secs: reported,
            slow_down,
        }) => {
            let floor = if slow_down {
                interval_secs.saturating_add(SLOW_DOWN_STEP_SECS)
            } else {
                interval_secs
            };
            keep(u64::from(reported).max(floor))
        }
        Err(BeamError::RateLimited { retry_after_secs }) => Next::PollAfter {
            wait_secs: retry_after_secs.max(interval_secs),
            interval_secs,
        },
        Err(
            BeamError::Server {
                retryable: true, ..
            }
            | BeamError::Network {
                retryable: true, ..
            },
        ) => keep(interval_secs),
        Err(failure) => Next::Fail(failure),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// Sign in with a device login [`Self::start_device_login`] began,
    /// polling until the viewer approves, refuses, or the code expires.
    ///
    /// The whole of RFC 8628's client side: the first poll waits the
    /// prompt's interval; `slow_down` adds five seconds to it; a rate limit
    /// waits its `Retry-After`; a retryable failure is polled through, since
    /// the viewer may be mid-approval. The flow ends at the prompt's
    /// `expires_in_secs` even where the server cannot be reached to say so.
    ///
    /// Cancelled by dropping the future -- a cancelled Kotlin coroutine or
    /// Swift task -- which stops polling before the next request.
    ///
    /// # Errors
    ///
    /// The first error [`Self::poll_device_login`] returns that a later poll
    /// could not get past: [`BeamError::Forbidden`] when the user refused,
    /// [`BeamError::Server`] with status 410 when the code expired (by the
    /// server's word or this device's clock), [`BeamError::BadRequest`] when
    /// the flow is unknown, and the failures of [`Self::complete_login`] once
    /// approved.
    pub async fn await_device_login(
        &self,
        server_id: String,
        prompt: DeviceLoginPrompt,
    ) -> Result<UserSummary, BeamError> {
        let DeviceLoginPrompt {
            device_handle,
            user_code: _,
            verification_uri: _,
            verification_uri_complete: _,
            expires_in_secs,
            interval_secs,
        } = prompt;
        let expires_at = self
            .clock
            .now_unix()
            .saturating_add(i64::try_from(expires_in_secs).unwrap_or(i64::MAX));
        let mut interval_secs = u64::from(interval_secs);
        let mut wait_secs = interval_secs;
        loop {
            self.clock.sleep_secs(wait_secs).await;
            if self.clock.now_unix() >= expires_at {
                return Err(BeamError::Server {
                    status: 410,
                    retryable: false,
                    detail: "the device login expired before it was approved".to_owned(),
                    code: DEVICE_LOGIN_EXPIRED.to_owned(),
                });
            }
            let polled = self
                .poll_device_login(server_id.clone(), device_handle.clone())
                .await;
            match next(interval_secs, polled) {
                Next::PollAfter {
                    wait_secs: wait,
                    interval_secs: interval,
                } => {
                    wait_secs = wait;
                    interval_secs = interval;
                }
                Next::Done(user) => return Ok(user),
                Next::Fail(failure) => return Err(failure),
            }
        }
    }
}
