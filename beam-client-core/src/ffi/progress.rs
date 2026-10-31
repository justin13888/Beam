//! Playback progress: the throttled report and the offline queue behind it.

use super::{BeamClient, parse_uuid};
use crate::error::BeamError;
use crate::progress::{ProgressOutcome, QueuedProgress, ThrottleDecision};
use crate::transport::TransportFailure;
use std::sync::Arc;

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// Report where the viewer is in a file.
    ///
    /// Applies the shared throttle, and persists anything that could not be
    /// sent so a lost connection does not lose the user's place.
    ///
    /// `force` bypasses the interval for pause, seek-end and player release.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `file_id` is not a UUID, and
    /// [`BeamError::Storage`] only when the retry queue itself cannot be
    /// written. A failed *send* is reported as [`ProgressOutcome::Queued`],
    /// not as an error: the position is safe, and a player should not surface
    /// a network blip as a playback failure.
    pub async fn report_progress(
        &self,
        file_id: String,
        position_secs: f64,
        duration_secs: Option<f64>,
        force: bool,
    ) -> Result<ProgressOutcome, BeamError> {
        let wire_file_id = parse_uuid("file_id", &file_id)?;
        let (server_id, client, _) = self.active_context()?;
        let (throttle, queue) = self.with_context(&server_id, |context| {
            (Arc::clone(&context.throttle), Arc::clone(&context.queue))
        })?;

        let (position, duration) =
            match throttle.decide(&file_id, position_secs, duration_secs, force) {
                ThrottleDecision::Hold {
                    next_eligible_in_secs,
                } => {
                    return Ok(ProgressOutcome::Throttled {
                        next_eligible_in_secs,
                    });
                }
                ThrottleDecision::Send {
                    position_secs,
                    duration_secs,
                } => (position_secs, duration_secs),
            };

        let body = crate::api::types::ReportProgressRequest {
            duration_secs: duration,
            position_secs: position,
        };
        match TransportFailure::capture(client.report_playback_progress(wire_file_id, &body)).await
        {
            Ok(response) => {
                queue.remove(&file_id).await?;
                Ok(ProgressOutcome::Sent {
                    position_secs: response.into_inner().position_secs,
                })
            }
            Err(failure) => {
                // The throttle already recorded this as sent, so the next
                // sample would be held back behind an interval that never
                // produced a request. Clearing it lets the retry happen.
                throttle.reset(&file_id);
                let mapped = self.fail(&server_id, &failure).await;

                // A rejected body, a forbidden file, a title that no longer
                // exists: the identical request fails identically forever.
                // Queuing one occupied a slot in a bounded queue and never
                // retired, because `enqueue` replaces the entry and resets
                // `attempts` -- so while a title played and sampled every
                // fifteen seconds, MAX_ATTEMPTS was reset before it could
                // count. `Dropped` existed for exactly this and had no
                // producer.
                //
                // Only a *permanent* refusal is dropped. An expired session,
                // a missing one, or a certificate the user has not yet trusted
                // is not retryable *now*, but the same sample is accepted the
                // moment the user signs in or trusts the certificate -- and
                // the whole point of the queue is that a resume point survives
                // exactly that kind of interruption. Those are queued, and
                // `flush_progress` sends them once the user has acted.
                if mapped.is_permanent() {
                    return Ok(ProgressOutcome::Dropped {
                        reason: mapped.to_string(),
                    });
                }

                // A 429 names the interval the server wants left alone. Queued
                // as due at once, the next `flush_progress` sent it straight
                // back into that interval -- the flush path already honoured
                // `Retry-After`, this one dropped it. Saturating, like
                // `record_failure`: the transport clamps the header, but this
                // layer must not be the one that panics if it ever does not.
                let now = self.clock.now_unix();
                let not_before_unix = failure.retry_after_secs.map_or(now, |seconds| {
                    now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX))
                });
                queue
                    .enqueue(QueuedProgress {
                        file_id,
                        position_secs: position,
                        duration_secs: duration,
                        captured_at_unix: now,
                        attempts: 0,
                        not_before_unix,
                    })
                    .await?;
                let pending = queue.len().await?;
                Ok(ProgressOutcome::Queued {
                    pending: u32::try_from(pending).unwrap_or(u32::MAX),
                })
            }
        }
    }

    /// Send every queued position that is due.
    ///
    /// Called on reconnect and at app start. Returns how many were accepted.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Storage`] when the queue cannot be read or written, and
    /// [`BeamError::SessionExpired`] when the server rejected the session
    /// part-way through. That second one is not incidental bookkeeping: the
    /// rejection takes the credential off the client and deletes the stored
    /// cookie, so a flush that answered with a count alone signed the device
    /// out and told the caller only how many samples went. Whatever was
    /// accepted before it has already left the queue, and
    /// [`Self::pending_progress_count`] says what is still waiting.
    pub async fn flush_progress(&self) -> Result<u32, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let queue = self.with_context(&server_id, |context| Arc::clone(&context.queue))?;

        let mut sent = 0_u32;
        for entry in queue.ready().await? {
            let Ok(wire_file_id) = uuid::Uuid::parse_str(&entry.file_id) else {
                // The path parameter is a UUID, so an entry that is not one
                // was written by an older build and can never be accepted.
                // It is treated exactly like a rejected send.
                queue.record_failure(&entry.file_id, None).await?;
                break;
            };
            let body = crate::api::types::ReportProgressRequest {
                duration_secs: entry.duration_secs,
                position_secs: entry.position_secs,
            };
            match TransportFailure::capture(client.report_playback_progress(wire_file_id, &body))
                .await
            {
                Ok(_) => {
                    queue.remove(&entry.file_id).await?;
                    sent = sent.saturating_add(1);
                }
                Err(failure) => {
                    // The server's own interval, where it sent one. Passing
                    // `None` here fell back to blind exponential backoff and
                    // ignored a 429's `Retry-After` -- which this crate reads,
                    // parses, and carried as far as this line before dropping.
                    queue
                        .record_failure(&entry.file_id, failure.retry_after_secs)
                        .await?;
                    // A queue drain stops at the first failure rather than
                    // hammering an unreachable server with the whole backlog.
                    // An expiry stops it *and says so*: this is a background
                    // call, and it has just signed the device out.
                    let mapped = self.fail(&server_id, &failure).await;
                    if matches!(mapped, BeamError::SessionExpired) {
                        return Err(mapped);
                    }
                    break;
                }
            }
        }
        Ok(sent)
    }

    /// How many positions are waiting to be sent.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// [`BeamError::Storage`] when the queue cannot be read.
    pub async fn pending_progress_count(&self) -> Result<u32, BeamError> {
        let server_id = self.require_active()?;
        let queue = self.with_context(&server_id, |context| Arc::clone(&context.queue))?;
        Ok(u32::try_from(queue.len().await?).unwrap_or(u32::MAX))
    }
}
