//! Playback telemetry: the events a player records, their flush, and the
//! operator's summary of them.

use super::{BeamClient, parse_uuid};
use crate::error::BeamError;
use crate::telemetry::{
    PlaybackEvent, PlaybackTelemetrySummary, StartFailureCause, SwitchTrigger, TELEMETRY_DISABLED,
    TelemetryClientKind, TelemetryFlush, batch_body,
};
use crate::transport::{ABOUT_BLANK, TransportFailure};
use std::sync::Arc;

/// A `YYYY-MM-DD` day, for a date query parameter.
fn parse_day(field: &str, value: &str) -> Result<crate::api::Date, BeamError> {
    time::Date::parse(value, &time::format_description::well_known::Iso8601::DATE)
        .map(crate::api::Date)
        .map_err(|_| BeamError::BadRequest {
            detail: format!("{field} is not a YYYY-MM-DD date"),
            code: ABOUT_BLANK.to_owned(),
        })
}

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// Which kind of client this is, for every playback telemetry report.
    ///
    /// Set once at start-up, beside the device profile. A client that never
    /// sets it reports as [`TelemetryClientKind::Other`].
    pub fn set_playback_client_kind(&self, kind: TelemetryClientKind) {
        *self.client_kind.write().expect("client kind lock") = kind;
    }

    /// A playback started: its first frame is on screen.
    ///
    /// `audio_track_index` is the index into the source's `audio_tracks` of
    /// the track that is playing. Every `record_*` call answers how many events
    /// are now waiting, so the player can flush once a request's worth has
    /// built up; zero once the server has said telemetry is off.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `file_id` is not a UUID, and
    /// [`BeamError::NoActiveServer`] when no server is selected.
    pub fn record_playback_start(
        &self,
        file_id: String,
        audio_track_index: Option<u32>,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(Some(PlaybackEvent::Start {
            file_id,
            audio_track_index,
        }))
    }

    /// A playback failed to start.
    ///
    /// A failure after the first frame is not a start failure and is not
    /// reported. A [`StartFailureCause::Rejected`] for a policy exclusion is
    /// accepted and not counted.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`].
    pub fn record_playback_start_failure(
        &self,
        file_id: String,
        cause: StartFailureCause,
        audio_track_index: Option<u32>,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(PlaybackEvent::start_failure(
            file_id,
            cause,
            audio_track_index,
        ))
    }

    /// Playback stalled mid-stream for `duration_ms`.
    ///
    /// Only a stall after the first frame, and never the buffering a seek or a
    /// source switch causes: those are the player filling an empty buffer on
    /// purpose. A zero-length stall is not counted; one over ten minutes is
    /// counted as ten minutes.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`].
    pub fn record_playback_rebuffer(
        &self,
        file_id: String,
        duration_ms: u64,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(PlaybackEvent::rebuffer(file_id, duration_ms))
    }

    /// Playback moved from one source of a title to another.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`], for either identifier.
    pub fn record_playback_source_switch(
        &self,
        from_file_id: String,
        to_file_id: String,
        trigger: SwitchTrigger,
    ) -> Result<u32, BeamError> {
        let from_file_id = parse_uuid("from_file_id", &from_file_id)?;
        let to_file_id = parse_uuid("to_file_id", &to_file_id)?;
        self.record_telemetry(PlaybackEvent::source_switch(
            from_file_id,
            to_file_id,
            trigger,
        ))
    }

    /// Report everything waiting, at most
    /// [`crate::telemetry::MAX_EVENTS_PER_REQUEST`] events to
    /// a request.
    ///
    /// Called on pause, on stop, when the app leaves the foreground, and once
    /// a `record_*` call says a request's worth is waiting. A 409 means the
    /// operator has telemetry off: the queue lets go of everything and stops
    /// for the rest of the session. A refused batch -- 422 above all -- is
    /// dropped, and its pointers logged, because the identical batch is
    /// refused identically. Anything transient stops the flush and keeps the
    /// batch for the next one.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// [`BeamError::SessionExpired`] when the server rejected the session
    /// part-way through -- which signs the device out, as it does for
    /// [`Self::flush_progress`]. Every other failure is an outcome, not an
    /// error: telemetry is never worth interrupting the viewer for.
    pub async fn flush_playback_telemetry(&self) -> Result<TelemetryFlush, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let telemetry = self.with_context(&server_id, |context| Arc::clone(&context.telemetry))?;
        let client_kind = *self.client_kind.read().expect("client kind lock");

        let mut outcome = TelemetryFlush::default();
        loop {
            let batch = telemetry.take_batch();
            if batch.is_empty() {
                break;
            }
            let size = u32::try_from(batch.len()).unwrap_or(u32::MAX);
            let body = batch_body(client_kind, &batch);
            let Err(failure) =
                TransportFailure::capture(client.report_playback_telemetry(&body)).await
            else {
                outcome.sent = outcome.sent.saturating_add(size);
                continue;
            };

            let mapped = self.fail(&server_id, &failure).await;
            match &mapped {
                BeamError::Server {
                    status: 409, code, ..
                } if code == TELEMETRY_DISABLED => {
                    telemetry.disable();
                    break;
                }
                refused if refused.is_permanent() => {
                    let pointers = failure
                        .problem
                        .as_ref()
                        .map(|problem| problem.pointers.join(", "))
                        .unwrap_or_default();
                    tracing::warn!(
                        server_id,
                        error = %refused,
                        pointers,
                        events = size,
                        "the server refused a playback telemetry batch; dropping it"
                    );
                    outcome.dropped = outcome.dropped.saturating_add(size);
                }
                transient => {
                    telemetry.restore(batch);
                    if matches!(transient, BeamError::SessionExpired) {
                        return Err(mapped);
                    }
                    break;
                }
            }
        }
        outcome.pending = telemetry.len();
        outcome.disabled = telemetry.is_disabled();
        Ok(outcome)
    }

    /// Playback telemetry for an inclusive range of UTC days, summed into the
    /// admin views.
    ///
    /// `from` and `to` are `YYYY-MM-DD`. Either may be omitted: the server
    /// then answers the thirty days ending today. Readable whether or not
    /// collection is on, so an operator can read what was kept after turning
    /// it off.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] for a date that does not parse or a range the
    /// server refuses, [`BeamError::Forbidden`] for a non-administrator, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn admin_playback_telemetry(
        &self,
        from: Option<String>,
        to: Option<String>,
    ) -> Result<PlaybackTelemetrySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetPlaybackTelemetryParams {
            from: from
                .as_deref()
                .map(|day| parse_day("from", day))
                .transpose()?,
            to: to.as_deref().map(|day| parse_day("to", day)).transpose()?,
        };
        let response = self
            .send(&server_id, client.get_playback_telemetry(params))
            .await?;
        Ok(PlaybackTelemetrySummary::from_report(response.into_inner()))
    }
}

impl BeamClient {
    /// Queue `event` for the active server, where there is one to queue.
    ///
    /// `None` is an event the vocabulary does not count -- a policy
    /// exclusion, a zero-length stall, a switch to the same file -- and
    /// changes nothing.
    fn record_telemetry(&self, event: Option<PlaybackEvent>) -> Result<u32, BeamError> {
        let server_id = self.require_active()?;
        self.with_context(&server_id, |context| match event {
            Some(event) => context.telemetry.record(event),
            None => context.telemetry.len(),
        })
    }
}
