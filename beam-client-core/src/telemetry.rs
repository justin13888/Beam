//! Playback telemetry: what started, what failed to start and why, what
//! rebuffered, and what switched source -- counted by the server, batched here.
//!
//! The server half is `POST /v1/telemetry/playback`, off by default behind the
//! operator's `BEAM_PLAYBACK_TELEMETRY_ENABLED` ([ADR-0019]). It keeps daily
//! counts under coarse dimensions of the file an event names and then forgets
//! the file, so nothing here is a viewing record once it arrives. This module
//! is everything a native client needs on its side of that, decided once:
//!
//! - **The vocabulary.** A capability rejection is mapped to the wire's
//!   failure reason by [`crate::RejectionReason::failure_reason`], and a
//!   player's own failure to [`FailureReason::Network`] or
//!   [`FailureReason::Other`], in [`StartFailureCause`]. A platform reports
//!   what happened and never picks a wire value itself.
//! - **The queue.** Events wait in a bounded, in-memory [`TelemetryQueue`] and
//!   leave at most [`MAX_EVENTS_PER_REQUEST`] to a request, which is the
//!   server's limit on a whole batch.
//! - **What an answer means.** A 409 is the operator saying no: the queue
//!   disables itself for the rest of the process and discards what it held.
//!   A 422 is a batch the server will refuse however often it is sent, so it
//!   is dropped. Anything transient puts the batch back for the next flush.
//!
//! The queue is deliberately not persisted. A lost batch costs a few counts in
//! an aggregate; persisting it would put a list of file identifiers the viewer
//! played on disk, which is exactly the record the server is careful not to
//! keep.
//!
//! [ADR-0019]: ../../docs/architecture/decisions/ADR-0019-telemetry-posture.md

use crate::api::types as wire;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

/// The most events one `POST /v1/telemetry/playback` may carry, across all
/// four lists together.
///
/// The server's own limit: `PlaybackTelemetryBatch` caps the total at 50 and
/// answers a larger batch with 422.
pub const MAX_EVENTS_PER_REQUEST: usize = 50;

/// The most events held between flushes.
///
/// Ten full requests. A client that cannot reach the server for a whole
/// evening of playback keeps the most recent of what it saw rather than
/// growing without bound; the oldest are dropped first.
pub const MAX_PENDING_EVENTS: usize = 10 * MAX_EVENTS_PER_REQUEST;

/// The longest rebuffer the server accepts, in milliseconds: ten minutes.
pub const MAX_REBUFFER_MS: u32 = 600_000;

/// The problem `type` the server answers with while the operator has playback
/// telemetry switched off.
pub const TELEMETRY_DISABLED: &str =
    "https://beam.justinchung.net/reference/errors/#playback-telemetry-disabled";

/// Why a playback start failed, as the server counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, uniffi::Enum)]
pub enum FailureReason {
    /// The device cannot open the container.
    Container,
    /// The device cannot decode the video stream.
    VideoCodec,
    /// The device cannot decode any audio track.
    AudioCodec,
    /// The bytes could not be fetched.
    Network,
    /// Anything else.
    Other,
}

/// Where a playback start failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FailureStage {
    /// Before anything was fetched: the capability check set the file aside.
    Preflight,
    /// In the player, once it had been handed the file.
    Playback,
}

/// What made the player move to a different source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SwitchTrigger {
    /// The viewer chose the other file.
    Manual,
    /// The client chose it, after judging rebuffering sustained.
    Auto,
}

/// Which native client is reporting.
///
/// The browser families the server also knows are absent: no browser links
/// this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Enum)]
pub enum TelemetryClientKind {
    /// An Android phone or tablet.
    Android,
    /// An Android TV device.
    AndroidTv,
    /// An iPhone or iPad.
    AppleIos,
    /// An Apple TV.
    AppleTvos,
    /// A Mac.
    AppleMacos,
    /// Anything else, and what a client that never said reports as.
    #[default]
    Other,
}

/// What stopped a playback from starting, as the platform observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum StartFailureCause {
    /// The core's capability check rejected the file before it was fetched.
    Rejected {
        /// The rejection, exactly as the check gave it.
        reason: crate::RejectionReason,
    },
    /// The player could not fetch the file.
    Network,
    /// The player failed for any other reason.
    Other,
}

impl StartFailureCause {
    /// The reason and stage this is counted under, or `None` when it is not
    /// a failure to count.
    #[must_use]
    pub const fn classify(self) -> Option<(FailureReason, FailureStage)> {
        match self {
            Self::Rejected { reason } => match reason.failure_reason() {
                Some(reason) => Some((reason, FailureStage::Preflight)),
                None => None,
            },
            Self::Network => Some((FailureReason::Network, FailureStage::Playback)),
            Self::Other => Some((FailureReason::Other, FailureStage::Playback)),
        }
    }
}

/// One event waiting to be reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackEvent {
    /// A playback that started.
    Start {
        /// The file that played.
        file_id: uuid::Uuid,
        /// Index into the file's `audio_tracks`, where known.
        audio_track_index: Option<u32>,
    },
    /// A playback that failed to start.
    StartFailure {
        /// The file that failed.
        file_id: uuid::Uuid,
        /// Why.
        reason: FailureReason,
        /// Where.
        stage: FailureStage,
        /// Index into the file's `audio_tracks`, where known.
        audio_track_index: Option<u32>,
    },
    /// A mid-stream stall.
    Rebuffer {
        /// The file that was playing.
        file_id: uuid::Uuid,
        /// How long it stalled, within the server's 1 ms to 10 minute range.
        duration_ms: u32,
    },
    /// A move from one source of a title to another.
    SourceSwitch {
        /// The file playback left.
        from_file_id: uuid::Uuid,
        /// The file it moved to; never `from_file_id`.
        to_file_id: uuid::Uuid,
        /// Who chose it.
        trigger: SwitchTrigger,
    },
}

impl PlaybackEvent {
    /// A start failure, or `None` for a cause that is not counted.
    #[must_use]
    pub const fn start_failure(
        file_id: uuid::Uuid,
        cause: StartFailureCause,
        audio_track_index: Option<u32>,
    ) -> Option<Self> {
        match cause.classify() {
            Some((reason, stage)) => Some(Self::StartFailure {
                file_id,
                reason,
                stage,
                audio_track_index,
            }),
            None => None,
        }
    }

    /// A rebuffer of `duration_ms`, or `None` for one too short to be one.
    ///
    /// A stall longer than [`MAX_REBUFFER_MS`] is held at it rather than
    /// discarded: it is still the longest kind of stall there is, and it lands
    /// in the same top bucket of the server's histogram either way.
    #[must_use]
    pub fn rebuffer(file_id: uuid::Uuid, duration_ms: u64) -> Option<Self> {
        if duration_ms == 0 {
            return None;
        }
        let duration_ms = u32::try_from(duration_ms)
            .unwrap_or(MAX_REBUFFER_MS)
            .min(MAX_REBUFFER_MS);
        Some(Self::Rebuffer {
            file_id,
            duration_ms,
        })
    }

    /// A source switch, or `None` when the two files are the same one --
    /// reloading a file is not a switch, and the server refuses it as one.
    #[must_use]
    pub fn source_switch(
        from_file_id: uuid::Uuid,
        to_file_id: uuid::Uuid,
        trigger: SwitchTrigger,
    ) -> Option<Self> {
        (from_file_id != to_file_id).then_some(Self::SourceSwitch {
            from_file_id,
            to_file_id,
            trigger,
        })
    }
}

/// Events waiting to be reported, for one server.
#[derive(Debug, Default)]
pub struct TelemetryQueue {
    state: Mutex<QueueState>,
}

#[derive(Debug, Default)]
struct QueueState {
    events: VecDeque<PlaybackEvent>,
    /// Set once the server has said telemetry is off. Never cleared: the
    /// operator's switch is read at the server's start, so asking again in the
    /// same session only earns the same answer.
    disabled: bool,
}

impl TelemetryQueue {
    /// An empty queue that will accept events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `event` for the next flush, and say how many are now waiting.
    ///
    /// Once the server has said telemetry is off this discards the event and
    /// answers zero, so a platform flushing whenever enough are waiting never
    /// asks again.
    pub fn record(&self, event: PlaybackEvent) -> u32 {
        let mut state = self.state.lock().expect("telemetry lock");
        if state.disabled {
            return 0;
        }
        state.events.push_back(event);
        while state.events.len() > MAX_PENDING_EVENTS {
            state.events.pop_front();
        }
        count(state.events.len())
    }

    /// Take the next request's worth, oldest first.
    ///
    /// Taken rather than read, so two flushes running at once send disjoint
    /// batches instead of the same one twice.
    pub fn take_batch(&self) -> Vec<PlaybackEvent> {
        let mut state = self.state.lock().expect("telemetry lock");
        let take = state.events.len().min(MAX_EVENTS_PER_REQUEST);
        state.events.drain(..take).collect()
    }

    /// Put back a batch that could not be delivered, ahead of anything
    /// recorded since, so the queue stays in the order events happened.
    pub fn restore(&self, batch: Vec<PlaybackEvent>) {
        let mut state = self.state.lock().expect("telemetry lock");
        if state.disabled {
            return;
        }
        for event in batch.into_iter().rev() {
            state.events.push_front(event);
        }
        while state.events.len() > MAX_PENDING_EVENTS {
            state.events.pop_front();
        }
    }

    /// The server said telemetry is off: stop for the rest of the session,
    /// and let go of everything held.
    pub fn disable(&self) {
        let mut state = self.state.lock().expect("telemetry lock");
        state.disabled = true;
        state.events.clear();
    }

    /// Whether the server has said telemetry is off.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.state.lock().expect("telemetry lock").disabled
    }

    /// How many events are waiting.
    #[must_use]
    pub fn len(&self) -> u32 {
        count(self.state.lock().expect("telemetry lock").events.len())
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.lock().expect("telemetry lock").events.is_empty()
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// The request body for one batch.
///
/// A list with nothing in it is omitted, which the server reads as empty.
#[must_use]
pub fn batch_body(
    client_kind: TelemetryClientKind,
    events: &[PlaybackEvent],
) -> wire::PlaybackTelemetryBatch {
    let mut starts = Vec::new();
    let mut start_failures = Vec::new();
    let mut rebuffers = Vec::new();
    let mut source_switches = Vec::new();

    for event in events {
        match *event {
            PlaybackEvent::Start {
                file_id,
                audio_track_index,
            } => starts.push(wire::PlaybackStartEvent {
                file_id,
                audio_track_index: audio_track_index.map(i64::from),
            }),
            PlaybackEvent::StartFailure {
                file_id,
                reason,
                stage,
                audio_track_index,
            } => start_failures.push(wire::PlaybackStartFailureEvent {
                file_id,
                reason: wire_reason(reason),
                stage: match stage {
                    FailureStage::Preflight => wire::PlaybackFailureStage::Preflight,
                    FailureStage::Playback => wire::PlaybackFailureStage::Playback,
                },
                audio_track_index: audio_track_index.map(i64::from),
            }),
            PlaybackEvent::Rebuffer {
                file_id,
                duration_ms,
            } => rebuffers.push(wire::PlaybackRebufferEvent {
                file_id,
                duration_ms: i64::from(duration_ms),
            }),
            PlaybackEvent::SourceSwitch {
                from_file_id,
                to_file_id,
                trigger,
            } => source_switches.push(wire::PlaybackSourceSwitchEvent {
                from_file_id,
                to_file_id,
                trigger: match trigger {
                    SwitchTrigger::Manual => wire::PlaybackSwitchTrigger::Manual,
                    SwitchTrigger::Auto => wire::PlaybackSwitchTrigger::Auto,
                },
            }),
        }
    }

    wire::PlaybackTelemetryBatch {
        client_kind: match client_kind {
            TelemetryClientKind::Android => wire::PlaybackClientKind::Android,
            TelemetryClientKind::AndroidTv => wire::PlaybackClientKind::AndroidTv,
            TelemetryClientKind::AppleIos => wire::PlaybackClientKind::AppleIos,
            TelemetryClientKind::AppleTvos => wire::PlaybackClientKind::AppleTvos,
            TelemetryClientKind::AppleMacos => wire::PlaybackClientKind::AppleMacos,
            TelemetryClientKind::Other => wire::PlaybackClientKind::Other,
        },
        starts: present(starts),
        start_failures: present(start_failures),
        rebuffers: present(rebuffers),
        source_switches: present(source_switches),
    }
}

fn present<T>(list: Vec<T>) -> Option<Vec<T>> {
    (!list.is_empty()).then_some(list)
}

const fn wire_reason(reason: FailureReason) -> wire::PlaybackFailureReason {
    match reason {
        FailureReason::Container => wire::PlaybackFailureReason::Container,
        FailureReason::VideoCodec => wire::PlaybackFailureReason::VideoCodec,
        FailureReason::AudioCodec => wire::PlaybackFailureReason::AudioCodec,
        FailureReason::Network => wire::PlaybackFailureReason::Network,
        FailureReason::Other => wire::PlaybackFailureReason::Other,
    }
}

/// What a flush did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, uniffi::Record)]
pub struct TelemetryFlush {
    /// Events the server accepted.
    pub sent: u32,
    /// Events dropped because the server refused the batch they were in.
    pub dropped: u32,
    /// Events still waiting, for the next flush.
    pub pending: u32,
    /// Whether the server has said telemetry is off. Recording is a no-op
    /// from then on.
    pub disabled: bool,
}

// -- the admin view ---------------------------------------------------------

/// The duration range one rebuffer fell into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, uniffi::Enum)]
pub enum RebufferBucket {
    /// Under one second.
    Under1Secs,
    /// One to three seconds.
    From1To3Secs,
    /// Three to ten seconds.
    From3To10Secs,
    /// Ten to thirty seconds.
    From10To30Secs,
    /// Thirty seconds or more.
    AtLeast30Secs,
}

impl RebufferBucket {
    /// Every bucket, shortest first.
    pub const ALL: [Self; 5] = [
        Self::Under1Secs,
        Self::From1To3Secs,
        Self::From3To10Secs,
        Self::From10To30Secs,
        Self::AtLeast30Secs,
    ];

    const fn from_wire(bucket: &wire::PlaybackRebufferBucket) -> Self {
        match bucket {
            wire::PlaybackRebufferBucket::Under1Secs => Self::Under1Secs,
            wire::PlaybackRebufferBucket::From1To3Secs => Self::From1To3Secs,
            wire::PlaybackRebufferBucket::From3To10Secs => Self::From3To10Secs,
            wire::PlaybackRebufferBucket::From10To30Secs => Self::From10To30Secs,
            wire::PlaybackRebufferBucket::AtLeast30Secs => Self::AtLeast30Secs,
        }
    }
}

/// Starts and start failures under one value of one dimension.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FailureRateRow {
    /// The dimension's value as the server names it: a codec such as `hevc`,
    /// or a client kind such as `android_tv`.
    pub label: String,
    /// Playbacks that started.
    pub started: u64,
    /// Playbacks that failed to start.
    pub failed: u64,
    /// `failed / (started + failed)`, from 0 to 1.
    pub failure_rate: f64,
}

/// How many rebuffers fell into one duration range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct RebufferBucketRow {
    /// The range.
    pub bucket: RebufferBucket,
    /// How many.
    pub count: u64,
}

/// Source switches under one combination of dimensions.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SourceSwitchRow {
    /// Which client kind switched, as the server names it.
    pub client_kind: String,
    /// Who chose the switch.
    pub trigger: SwitchTrigger,
    /// The resolution class left (`sd`, `hd`, `fhd`, `uhd`, `unknown`).
    pub from_height_class: String,
    /// The resolution class moved to.
    pub to_height_class: String,
    /// How many.
    pub count: u64,
}

/// The playback telemetry an operator reads, for a range of days.
///
/// The server's report summed into the three views the admin screen shows:
/// failure rate by codec and by client, the rebuffer histogram, and source
/// switches. Summed here once rather than in each platform's screen.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlaybackTelemetrySummary {
    /// Whether the server is counting reports now.
    pub enabled: bool,
    /// First day counted, `YYYY-MM-DD`, UTC.
    pub from: String,
    /// Last day counted, inclusive.
    pub to: String,
    /// How many days the server keeps counts.
    pub retention_days: u32,
    /// Playbacks that started.
    pub started: u64,
    /// Playbacks that failed to start.
    pub failed: u64,
    /// Mid-stream stalls.
    pub rebuffers: u64,
    /// Their summed duration.
    pub rebuffer_total_ms: u64,
    /// Source switches.
    pub source_switches: u64,
    /// By the file's first video stream's codec, most attempts first.
    pub by_video_codec: Vec<FailureRateRow>,
    /// By the audio codec that played or failed, most attempts first.
    pub by_audio_codec: Vec<FailureRateRow>,
    /// By client kind, most attempts first.
    pub by_client: Vec<FailureRateRow>,
    /// Every duration range, shortest first, including empty ones.
    pub rebuffer_histogram: Vec<RebufferBucketRow>,
    /// Source switches, largest first.
    pub switches: Vec<SourceSwitchRow>,
}

impl PlaybackTelemetrySummary {
    /// Sum the server's report into the admin views.
    #[must_use]
    pub fn from_report(report: wire::PlaybackTelemetryReport) -> Self {
        let wire::PlaybackTelemetryReport {
            enabled,
            from,
            to,
            retention_days,
            totals,
            starts,
            start_failures,
            rebuffers,
            source_switches,
        } = report;

        let mut by_video_codec = Tally::default();
        let mut by_audio_codec = Tally::default();
        let mut by_client = Tally::default();
        for start in &starts {
            let count = unsigned(start.count);
            by_video_codec.started(&start.video_codec, count);
            by_audio_codec.started(&start.audio_codec, count);
            by_client.started(&start.client_kind.to_string(), count);
        }
        for failure in &start_failures {
            let count = unsigned(failure.count);
            by_video_codec.failed(&failure.video_codec, count);
            by_audio_codec.failed(&failure.audio_codec, count);
            by_client.failed(&failure.client_kind.to_string(), count);
        }

        let mut histogram: BTreeMap<RebufferBucket, u64> = RebufferBucket::ALL
            .into_iter()
            .map(|bucket| (bucket, 0))
            .collect();
        for row in &rebuffers {
            for bucket in &row.histogram {
                *histogram
                    .entry(RebufferBucket::from_wire(&bucket.bucket))
                    .or_default() += unsigned(bucket.count);
            }
        }

        let mut switches: Vec<SourceSwitchRow> = source_switches
            .into_iter()
            .map(|row| SourceSwitchRow {
                client_kind: row.client_kind.to_string(),
                trigger: match row.trigger {
                    wire::PlaybackSwitchTrigger::Manual => SwitchTrigger::Manual,
                    wire::PlaybackSwitchTrigger::Auto => SwitchTrigger::Auto,
                },
                from_height_class: row.from_height_class.to_string(),
                to_height_class: row.to_height_class.to_string(),
                count: unsigned(row.count),
            })
            .collect();
        // Stable on the server's own order for equal counts.
        switches.sort_by(|a, b| b.count.cmp(&a.count));

        Self {
            enabled,
            from: from.0.to_string(),
            to: to.0.to_string(),
            retention_days: u32::try_from(retention_days).unwrap_or(u32::MAX),
            started: unsigned(totals.started_count),
            failed: unsigned(totals.failed_count),
            rebuffers: unsigned(totals.rebuffer_count),
            rebuffer_total_ms: unsigned(totals.rebuffer_total_ms),
            source_switches: unsigned(totals.source_switch_count),
            by_video_codec: by_video_codec.rows(),
            by_audio_codec: by_audio_codec.rows(),
            by_client: by_client.rows(),
            rebuffer_histogram: histogram
                .into_iter()
                .map(|(bucket, count)| RebufferBucketRow { bucket, count })
                .collect(),
            switches,
        }
    }
}

/// A count the generated client decodes as `i64` although the schema says it
/// is never negative.
fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Starts and failures per label.
#[derive(Default)]
struct Tally(BTreeMap<String, (u64, u64)>);

impl Tally {
    fn started(&mut self, label: &str, count: u64) {
        let entry = self.0.entry(label.to_owned()).or_default();
        entry.0 = entry.0.saturating_add(count);
    }

    fn failed(&mut self, label: &str, count: u64) {
        let entry = self.0.entry(label.to_owned()).or_default();
        entry.1 = entry.1.saturating_add(count);
    }

    /// Most attempts first, then by label, so the order is stable.
    fn rows(self) -> Vec<FailureRateRow> {
        let mut rows: Vec<FailureRateRow> = self
            .0
            .into_iter()
            .map(|(label, (started, failed))| {
                let attempts = started.saturating_add(failed);
                #[allow(clippy::cast_precision_loss)]
                let failure_rate = if attempts == 0 {
                    0.0
                } else {
                    failed as f64 / attempts as f64
                };
                FailureRateRow {
                    label,
                    started,
                    failed,
                    failure_rate,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            (b.started + b.failed)
                .cmp(&(a.started + a.failed))
                .then_with(|| a.label.cmp(&b.label))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RejectionReason;

    fn file(n: u8) -> uuid::Uuid {
        uuid::Uuid::from_bytes([n; 16])
    }

    fn start(n: u8) -> PlaybackEvent {
        PlaybackEvent::Start {
            file_id: file(n),
            audio_track_index: None,
        }
    }

    #[test]
    fn a_policy_exclusion_is_not_a_failure() {
        assert_eq!(
            PlaybackEvent::start_failure(
                file(1),
                StartFailureCause::Rejected {
                    reason: RejectionReason::ExcludedByPolicy,
                },
                None,
            ),
            None,
            "a quality preference must not be counted as an incompatibility"
        );
    }

    #[test]
    fn a_capability_rejection_is_a_preflight_failure_and_a_player_failure_is_not() {
        let rejected = PlaybackEvent::start_failure(
            file(1),
            StartFailureCause::Rejected {
                reason: RejectionReason::HdrUnsupported,
            },
            Some(1),
        );
        assert_eq!(
            rejected,
            Some(PlaybackEvent::StartFailure {
                file_id: file(1),
                reason: FailureReason::VideoCodec,
                stage: FailureStage::Preflight,
                audio_track_index: Some(1),
            })
        );

        for cause in [StartFailureCause::Network, StartFailureCause::Other] {
            assert!(matches!(
                PlaybackEvent::start_failure(file(1), cause, None),
                Some(PlaybackEvent::StartFailure {
                    stage: FailureStage::Playback,
                    ..
                })
            ));
        }
    }

    #[test]
    fn every_counted_rejection_is_counted_at_preflight() {
        // Derived from the mapping rather than restating it: whatever a
        // rejection maps to, it is never `network` or `other`, which belong to
        // the player, and it is always counted before playback began.
        for reason in [
            RejectionReason::ContainerUnsupported,
            RejectionReason::VideoCodecUnsupported,
            RejectionReason::AudioCodecUnsupported,
            RejectionReason::ResolutionExceedsDecoder,
            RejectionReason::BitrateExceedsDecoder,
            RejectionReason::HdrUnsupported,
            RejectionReason::NoVideoStream,
            RejectionReason::SoftwareDecodeDisabled,
            RejectionReason::DiscStructureUnsupported,
        ] {
            let (counted, stage) = StartFailureCause::Rejected { reason }
                .classify()
                .unwrap_or_else(|| panic!("{reason:?} is a real incompatibility"));
            assert_eq!(stage, FailureStage::Preflight, "{reason:?}");
            assert!(
                !matches!(counted, FailureReason::Network | FailureReason::Other),
                "{reason:?} is a property of the file, not of the player"
            );
        }
    }

    #[test]
    fn a_rebuffer_is_held_within_the_range_the_server_accepts() {
        assert_eq!(PlaybackEvent::rebuffer(file(1), 0), None);
        assert_eq!(
            PlaybackEvent::rebuffer(file(1), 1),
            Some(PlaybackEvent::Rebuffer {
                file_id: file(1),
                duration_ms: 1
            })
        );
        for long in [u64::from(MAX_REBUFFER_MS) + 1, u64::MAX] {
            assert_eq!(
                PlaybackEvent::rebuffer(file(1), long),
                Some(PlaybackEvent::Rebuffer {
                    file_id: file(1),
                    duration_ms: MAX_REBUFFER_MS
                })
            );
        }
    }

    #[test]
    fn reloading_the_same_file_is_not_a_switch() {
        assert_eq!(
            PlaybackEvent::source_switch(file(1), file(1), SwitchTrigger::Manual),
            None
        );
        assert!(PlaybackEvent::source_switch(file(1), file(2), SwitchTrigger::Auto).is_some());
    }

    #[test]
    fn a_batch_never_exceeds_the_servers_limit_and_keeps_the_order() {
        let queue = TelemetryQueue::new();
        for n in 0..120_u8 {
            queue.record(start(n));
        }

        let first = queue.take_batch();
        assert_eq!(first.len(), MAX_EVENTS_PER_REQUEST);
        assert_eq!(first[0], start(0));
        assert_eq!(queue.take_batch().len(), MAX_EVENTS_PER_REQUEST);
        assert_eq!(queue.take_batch().len(), 20);
        assert!(queue.take_batch().is_empty());
        assert!(queue.is_empty());
    }

    #[test]
    fn a_restored_batch_goes_back_ahead_of_newer_events() {
        let queue = TelemetryQueue::new();
        queue.record(start(1));
        queue.record(start(2));
        let batch = queue.take_batch();
        queue.record(start(3));

        queue.restore(batch);

        assert_eq!(queue.take_batch(), vec![start(1), start(2), start(3)]);
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let queue = TelemetryQueue::new();
        queue.record(start(0));
        for _ in 0..MAX_PENDING_EVENTS {
            queue.record(start(1));
        }

        assert_eq!(queue.len() as usize, MAX_PENDING_EVENTS);
        assert_eq!(
            queue.take_batch()[0],
            start(1),
            "the oldest event is the one let go"
        );
    }

    #[test]
    fn a_disabled_queue_lets_go_and_accepts_nothing() {
        let queue = TelemetryQueue::new();
        queue.record(start(1));
        let batch = queue.take_batch();
        queue.record(start(2));

        queue.disable();
        queue.restore(batch);

        assert!(queue.is_disabled());
        assert_eq!(queue.record(start(3)), 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_body_omits_the_lists_it_has_nothing_for() {
        let body = batch_body(TelemetryClientKind::AndroidTv, &[start(1)]);
        let json = serde_json::to_value(&body).expect("serialisable");

        assert_eq!(json["client_kind"], "android_tv");
        assert_eq!(json["starts"][0]["file_id"], file(1).to_string());
        assert!(json.get("start_failures").is_none());
        assert!(json.get("rebuffers").is_none());
        assert!(json.get("source_switches").is_none());
    }

    #[test]
    fn a_body_carries_each_event_in_its_own_list() {
        let events = [
            PlaybackEvent::start_failure(file(1), StartFailureCause::Network, Some(2))
                .expect("counted"),
            PlaybackEvent::rebuffer(file(1), 1_500).expect("counted"),
            PlaybackEvent::source_switch(file(1), file(2), SwitchTrigger::Auto).expect("counted"),
        ];
        let json = serde_json::to_value(batch_body(TelemetryClientKind::Android, &events))
            .expect("serialisable");

        assert_eq!(
            json["start_failures"][0],
            serde_json::json!({
                "file_id": file(1).to_string(),
                "reason": "network",
                "stage": "playback",
                "audio_track_index": 2,
            })
        );
        assert_eq!(json["rebuffers"][0]["duration_ms"], 1_500);
        assert_eq!(json["source_switches"][0]["trigger"], "auto");
        assert!(json.get("starts").is_none());
    }

    fn report(value: serde_json::Value) -> wire::PlaybackTelemetryReport {
        serde_json::from_value(value).expect("a well-formed report")
    }

    fn empty_report() -> serde_json::Value {
        serde_json::json!({
            "enabled": true,
            "from": "2026-09-24",
            "to": "2026-10-23",
            "retention_days": 400,
            "totals": {
                "started_count": 0, "failed_count": 0, "rebuffer_count": 0,
                "rebuffer_total_ms": 0, "source_switch_count": 0,
            },
            "starts": [],
            "start_failures": [],
            "rebuffers": [],
            "source_switches": [],
        })
    }

    #[test]
    fn failure_rates_join_starts_and_failures_on_the_same_dimension() {
        let mut value = empty_report();
        value["starts"] = serde_json::json!([
            { "client_kind": "android", "container": "matroska", "video_codec": "hevc",
              "audio_codec": "aac", "height_class": "fhd", "count": 3 },
            { "client_kind": "android_tv", "container": "mp4", "video_codec": "h264",
              "audio_codec": "aac", "height_class": "hd", "count": 5 },
        ]);
        value["start_failures"] = serde_json::json!([
            { "client_kind": "android", "reason": "video_codec", "stage": "preflight",
              "container": "matroska", "video_codec": "hevc", "audio_codec": "truehd",
              "height_class": "uhd", "count": 1 },
        ]);

        let summary = PlaybackTelemetrySummary::from_report(report(value));

        let hevc = summary
            .by_video_codec
            .iter()
            .find(|row| row.label == "hevc")
            .expect("hevc row");
        assert_eq!((hevc.started, hevc.failed), (3, 1));
        assert!((hevc.failure_rate - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            summary.by_video_codec[0].label, "h264",
            "most attempts first"
        );

        let truehd = summary
            .by_audio_codec
            .iter()
            .find(|row| row.label == "truehd")
            .expect("a codec that only ever failed still has a row");
        assert_eq!((truehd.started, truehd.failed), (0, 1));
        assert!((truehd.failure_rate - 1.0).abs() < f64::EPSILON);

        let labels: Vec<_> = summary.by_client.iter().map(|row| &row.label).collect();
        assert_eq!(labels, ["android_tv", "android"]);
    }

    #[test]
    fn the_histogram_sums_every_row_and_lists_every_bucket() {
        let mut value = empty_report();
        let row = |client: &str, short: i64, long: i64| {
            serde_json::json!({
                "client_kind": client, "container": "mp4", "video_codec": "h264",
                "height_class": "hd", "bitrate_class": "from_2_to_8_mbps",
                "event_count": short + long, "total_ms": 0,
                "histogram": [
                    { "bucket": "under_1_secs", "count": short },
                    { "bucket": "from_1_to_3_secs", "count": 0 },
                    { "bucket": "from_3_to_10_secs", "count": 0 },
                    { "bucket": "from_10_to_30_secs", "count": 0 },
                    { "bucket": "at_least_30_secs", "count": long },
                ],
            })
        };
        value["rebuffers"] = serde_json::json!([row("android", 2, 1), row("apple_ios", 4, 0)]);

        let summary = PlaybackTelemetrySummary::from_report(report(value));

        let buckets: Vec<_> = summary
            .rebuffer_histogram
            .iter()
            .map(|row| row.bucket)
            .collect();
        assert_eq!(buckets, RebufferBucket::ALL);
        assert_eq!(summary.rebuffer_histogram[0].count, 6);
        assert_eq!(summary.rebuffer_histogram[4].count, 1);
    }

    #[test]
    fn an_empty_report_still_has_a_whole_histogram() {
        let summary = PlaybackTelemetrySummary::from_report(report(empty_report()));
        assert_eq!(summary.from, "2026-09-24");
        assert_eq!(summary.to, "2026-10-23");
        assert_eq!(summary.rebuffer_histogram.len(), RebufferBucket::ALL.len());
        assert!(summary.rebuffer_histogram.iter().all(|row| row.count == 0));
        assert!(summary.by_client.is_empty());
    }

    #[test]
    fn switches_are_listed_largest_first() {
        let mut value = empty_report();
        value["source_switches"] = serde_json::json!([
            { "client_kind": "android", "trigger": "manual", "from_height_class": "uhd",
              "to_height_class": "fhd", "count": 1 },
            { "client_kind": "android", "trigger": "auto", "from_height_class": "fhd",
              "to_height_class": "hd", "count": 4 },
        ]);

        let summary = PlaybackTelemetrySummary::from_report(report(value));

        assert_eq!(summary.switches[0].trigger, SwitchTrigger::Auto);
        assert_eq!(summary.switches[0].count, 4);
        assert_eq!(summary.switches[1].from_height_class, "uhd");
    }
}
