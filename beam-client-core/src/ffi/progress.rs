//! Playback progress: the throttled report and the offline queue behind it.

use super::{BeamClient, parse_uuid};
use crate::catalog::start_position_secs;
use crate::error::BeamError;
use crate::progress::{
    LastPositions, ProgressDrain, ProgressOutcome, ProgressQueue, QueuedProgress, SendFailure,
    ThrottleDecision, backoff_secs,
};
use crate::session::SessionState;
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
    /// [`BeamError::Storage`] only when the retry queue, or the device's own
    /// record of the position, cannot be written. A failed *send* is reported
    /// as [`ProgressOutcome::Queued`], not as an error: the position is safe, and a player should not surface
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
        // Whatever becomes of the send, the position is also recorded on the
        // device, so a Play with no network can resume from it later
        // (`local_start_position_secs`, issue #487): a sample the server
        // acknowledged leaves the queue, and with it the only copy the device
        // had. Recorded after the send, so the device's storage never holds
        // back the position the server keeps, and after the queue retires an
        // acknowledged sample, so a failure here cannot leave an older one
        // queued to overwrite it. On a failed send it is recorded after the
        // sample is queued, so a failure here cannot lose the retry.
        let recorded = self.last_positions(&server_id);
        match TransportFailure::capture(client.report_playback_progress(wire_file_id, &body)).await
        {
            Ok(response) => {
                queue.remove(&file_id).await?;
                recorded.record(&file_id, position, duration).await?;
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
                // its counts -- so while a title played and sampled every
                // fifteen seconds, the retirement cap was reset before it
                // could count. `Dropped` existed for exactly this and had no
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
                    recorded.record(&file_id, position, duration).await?;
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
                // Queued before it is recorded: the queued sample is the one
                // copy bound for the server, so a record the device cannot
                // write must not cost it. The queue also answers an offline
                // Play, so the position survives even then.
                queue
                    .enqueue(QueuedProgress {
                        file_id: file_id.clone(),
                        position_secs: position,
                        duration_secs: duration,
                        captured_at_unix: now,
                        attempts: 0,
                        rejections: 0,
                        not_before_unix,
                    })
                    .await?;
                recorded.record(&file_id, position, duration).await?;
                let pending = queue.len().await?;
                Ok(ProgressOutcome::Queued {
                    pending: u32::try_from(pending).unwrap_or(u32::MAX),
                })
            }
        }
    }

    /// Send every queued position that is due to the active server.
    ///
    /// Returns how many were accepted. [`Self::drain_progress`] sends every
    /// server's queue and says when to retry; this is the active server's
    /// alone, for a platform that has not moved to it yet.
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
        let server_id = self.require_active()?;
        let ServerDrain {
            sent,
            stopped: _,
            stopped_until: _,
            expired,
        } = self.drain_server(&server_id).await?;
        // An expiry stops the drain *and says so*: this is a background call,
        // and it has just signed the device out.
        if expired {
            return Err(BeamError::SessionExpired);
        }
        Ok(sent)
    }

    /// Send every queued position that is due, to the server that owns it.
    ///
    /// Each server's queue goes to that server, with its own session,
    /// whichever server is active: a position recorded offline against one
    /// server is not stranded by switching to another before connectivity
    /// returns. Servers that are not signed in are left alone -- their
    /// samples wait for the sign-in -- and one server failing does not stop
    /// the others from draining.
    ///
    /// The answer says when another drain is worth running, from the
    /// back-off the queue already applied to each failed entry (and any
    /// `Retry-After` a server sent), so a platform retries on the core's
    /// schedule and keeps none of its own. A second drain at the same instant
    /// sends nothing: a server that answered has nothing left due, and one
    /// that did not is only asked again for the entry behind the one that
    /// failed -- which is why the answer names a wait.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when a queue cannot be read or written,
    /// which backs nothing off: retry after
    /// [`crate::progress::progress_retry_delay_secs`]. A session the server
    /// refused is not an error here -- the drain carries on with the other
    /// servers and names it in [`ProgressDrain::expired_server_ids`].
    pub async fn drain_progress(&self) -> Result<ProgressDrain, BeamError> {
        let (mut signed_in, queues) = {
            let servers = self.servers.read().expect("servers lock");
            let signed_in: Vec<String> = servers
                .iter()
                .filter(|(_, context)| matches!(context.state, SessionState::Authenticated { .. }))
                .map(|(server_id, _)| server_id.clone())
                .collect();
            let queues: Vec<Arc<ProgressQueue>> = servers
                .values()
                .map(|context| Arc::clone(&context.queue))
                .collect();
            (signed_in, queues)
        };
        // A stable order, so which server is tried first does not depend on
        // a hash map's iteration.
        signed_in.sort();

        let mut sent = 0_u32;
        let mut wake: Option<i64> = None;
        let mut expired_server_ids = Vec::new();
        for server_id in signed_in {
            let ServerDrain {
                sent: sent_here,
                stopped,
                stopped_until,
                expired,
            } = self.drain_server(&server_id).await?;
            sent = sent.saturating_add(sent_here);
            if expired {
                // Nothing more goes to it until the user signs in again,
                // which is the platform's trigger, not a retry.
                expired_server_ids.push(server_id);
                continue;
            }
            let next = if stopped {
                // The entries behind the one that failed are still due, but
                // not worth sending before the server that just failed has
                // had the failed entry's back-off to recover. Where that
                // failure retired the entry, the queue's first step stands in.
                Some(
                    stopped_until
                        .unwrap_or_else(|| self.clock.now_unix().saturating_add(backoff_secs(1))),
                )
            } else {
                self.with_context(&server_id, |context| Arc::clone(&context.queue))?
                    .next_due_unix()
                    .await?
            };
            wake = match (wake, next) {
                (Some(current), Some(candidate)) => Some(current.min(candidate)),
                (current, candidate) => current.or(candidate),
            };
        }

        let mut pending = 0_usize;
        for queue in queues {
            pending = pending.saturating_add(queue.len().await?);
        }
        let now = self.clock.now_unix();
        Ok(ProgressDrain {
            sent,
            pending: u32::try_from(pending).unwrap_or(u32::MAX),
            // At least a second: an entry that fell due while this drain ran
            // must not turn the platform's retry into a busy loop.
            retry_in_secs: wake
                .map(|due| u64::try_from(due.saturating_sub(now)).unwrap_or(0).max(1)),
            expired_server_ids,
        })
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

    /// Where Play starts `file_id` from the last position this device
    /// recorded for it, by the start rule ([`start_position_secs`]): 0 where
    /// it recorded none, or one near the end.
    ///
    /// For Play with no network (issue #487), when the server's own position
    /// cannot be asked for. Needs no active server or session, and asks
    /// nothing of the network. The newest position wins across every known
    /// server -- the one queued unsent, or the one recorded at the last
    /// report -- because a file's id is unique and a download may have been
    /// reported while another server was active.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when a record cannot be read.
    pub async fn local_start_position_secs(&self, file_id: String) -> Result<f64, BeamError> {
        let contexts: Vec<(String, Arc<ProgressQueue>)> = {
            let servers = self.servers.read().expect("servers lock");
            servers
                .iter()
                .map(|(server_id, context)| (server_id.clone(), Arc::clone(&context.queue)))
                .collect()
        };

        // (captured at, position, duration) of the newest position seen.
        let mut newest: Option<(i64, f64, Option<f64>)> = None;
        for (server_id, queue) in contexts {
            let recorded = self
                .last_positions(&server_id)
                .get(&file_id)
                .await?
                .map(|entry| {
                    (
                        entry.captured_at_unix,
                        entry.position_secs,
                        entry.duration_secs,
                    )
                });
            let queued = queue.find(&file_id).await?.map(|entry| {
                (
                    entry.captured_at_unix,
                    entry.position_secs,
                    entry.duration_secs,
                )
            });
            for candidate in [recorded, queued].into_iter().flatten() {
                if newest.is_none_or(|(at, _, _)| candidate.0 > at) {
                    newest = Some(candidate);
                }
            }
        }
        Ok(newest.map_or(0.0, |(_, position, duration)| {
            start_position_secs(position, duration)
        }))
    }
}

/// What draining one server's queue did.
struct ServerDrain {
    /// Samples the server accepted.
    sent: u32,
    /// Whether a failed send stopped the drain before the due entries ran out.
    stopped: bool,
    /// When the entry whose failure stopped the drain is next due; `None`
    /// where that failure retired it, or nothing stopped the drain.
    stopped_until: Option<i64>,
    /// Whether the server refused the session part-way through.
    expired: bool,
}

impl BeamClient {
    /// The last position recorded for each file played from `server_id`.
    fn last_positions(&self, server_id: &str) -> LastPositions {
        LastPositions::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.clock),
            server_id,
        )
    }

    /// Send `server_id`'s due samples, oldest first, with its own client.
    ///
    /// Stops at the first failure rather than hammering an unreachable server
    /// with the whole backlog.
    async fn drain_server(&self, server_id: &str) -> Result<ServerDrain, BeamError> {
        let (client, queue) = self.with_context(server_id, |context| {
            (context.client.clone(), Arc::clone(&context.queue))
        })?;

        let mut drain = ServerDrain {
            sent: 0,
            stopped: false,
            stopped_until: None,
            expired: false,
        };
        for entry in queue.ready().await? {
            let Ok(wire_file_id) = uuid::Uuid::parse_str(&entry.file_id) else {
                // The path parameter is a UUID, so an entry that is not one
                // was written by an older build and can never be accepted.
                // It is treated exactly like a rejected send.
                drain.stopped = true;
                drain.stopped_until = queue
                    .record_failure(&entry.file_id, SendFailure::Rejected, None)
                    .await?;
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
                    drain.sent = drain.sent.saturating_add(1);
                }
                Err(failure) => {
                    let mapped = self.fail(server_id, &failure).await;
                    // Only a refusal of this sample counts toward retiring
                    // it. Every attempt used to count, so a server down for
                    // longer than eight backoff steps -- about four and a
                    // half minutes -- cost the viewer the very resume point
                    // the queue exists to keep. An outage, a 5xx (which is
                    // what a reverse proxy answers while the server is
                    // down), a rate limit or a sign-out says nothing about
                    // the sample; `MAX_AGE_SECS` bounds how long it waits.
                    let kind = if mapped.is_permanent() {
                        SendFailure::Rejected
                    } else {
                        SendFailure::Undelivered
                    };
                    // The server's own interval, where it sent one. Passing
                    // `None` here fell back to blind exponential backoff and
                    // ignored a 429's `Retry-After` -- which this crate reads,
                    // parses, and carried as far as this line before dropping.
                    drain.stopped = true;
                    drain.stopped_until = queue
                        .record_failure(&entry.file_id, kind, failure.retry_after_secs)
                        .await?;
                    drain.expired = matches!(mapped, BeamError::SessionExpired);
                    break;
                }
            }
        }
        Ok(drain)
    }
}
