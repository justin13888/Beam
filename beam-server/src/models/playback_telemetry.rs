//! Playback telemetry on the wire (issue #143, ADR-0019): the batch a client
//! reports, and the aggregate report an admin reads.
//!
//! A reported event names a file; the report never does. The server resolves
//! each file to coarse dimensions and keeps only daily counts, so there is no
//! field below -- in the report or anywhere after ingest -- that could hold a
//! user, file or title identifier.

use beam_domain::models::playback_telemetry::{
    BitrateClass, ClientKind, FailureReason, FailureStage, HeightClass, RebufferBucket,
    SwitchTrigger,
};
use chrono::NaiveDate;
use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The most events of any one kind a batch may carry, and the most in total.
pub const MAX_BATCH_EVENTS: usize = 50;

/// The longest rebuffer a client may report: ten minutes. Anything longer is
/// not a stall the viewer sat through.
pub const MAX_REBUFFER_MS: u32 = 600_000;

/// A wire enum over one domain vocabulary, and the conversions both ways.
///
/// The wire names are the domain's labels, written once per value here and
/// checked against the domain's own table by `wire_names_are_the_domain_labels`
/// below, so the API and the stored column values cannot drift apart.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $wire:ident <=> $domain:ident {
            $( $(#[$variant_meta:meta])* $variant:ident = $label:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Schema)]
        pub enum $wire {
            $( $(#[$variant_meta])* #[serde(rename = $label)] $variant ),+
        }

        impl From<$domain> for $wire {
            fn from(value: $domain) -> Self {
                match value {
                    $($domain::$variant => Self::$variant),+
                }
            }
        }

        impl From<$wire> for $domain {
            fn from(value: $wire) -> Self {
                match value {
                    $($wire::$variant => Self::$variant),+
                }
            }
        }
    };
}

wire_enum! {
    /// What kind of client reported: a browser family or a native platform,
    /// never a version or user agent.
    PlaybackClientKind <=> ClientKind {
        WebChromium = "web_chromium",
        WebFirefox = "web_firefox",
        WebSafari = "web_safari",
        WebOther = "web_other",
        Android = "android",
        AndroidTv = "android_tv",
        AppleIos = "apple_ios",
        AppleTvos = "apple_tvos",
        AppleMacos = "apple_macos",
        Other = "other",
    }
}

wire_enum! {
    /// Why a playback start failed.
    PlaybackFailureReason <=> FailureReason {
        /// The client cannot play the container.
        Container = "container",
        /// The client cannot decode the video stream.
        VideoCodec = "video_codec",
        /// The client cannot decode the chosen audio stream.
        AudioCodec = "audio_codec",
        /// The bytes did not arrive.
        Network = "network",
        /// Anything else.
        Other = "other",
    }
}

wire_enum! {
    /// Where a playback start failed.
    PlaybackFailureStage <=> FailureStage {
        /// The client ruled the source out before trying it.
        Preflight = "preflight",
        /// The player tried the source and failed.
        Playback = "playback",
    }
}

wire_enum! {
    /// The vertical resolution class of a file's video: `sd` under 720
    /// lines, `hd` from 720, `fhd` from 1080, `uhd` from 2160.
    PlaybackHeightClass <=> HeightClass {
        Sd = "sd",
        Hd = "hd",
        Fhd = "fhd",
        Uhd = "uhd",
        /// No video stream, or no recorded height.
        Unknown = "unknown",
    }
}

wire_enum! {
    /// The average bitrate class of a whole file.
    PlaybackBitrateClass <=> BitrateClass {
        Under2Mbps = "under_2_mbps",
        From2To8Mbps = "from_2_to_8_mbps",
        From8To20Mbps = "from_8_to_20_mbps",
        From20To50Mbps = "from_20_to_50_mbps",
        AtLeast50Mbps = "at_least_50_mbps",
        /// Neither the file's duration nor a stream bitrate was recorded.
        Unknown = "unknown",
    }
}

wire_enum! {
    /// The duration range one rebuffer fell into.
    PlaybackRebufferBucket <=> RebufferBucket {
        Under1Secs = "under_1_secs",
        From1To3Secs = "from_1_to_3_secs",
        From3To10Secs = "from_3_to_10_secs",
        From10To30Secs = "from_10_to_30_secs",
        AtLeast30Secs = "at_least_30_secs",
    }
}

wire_enum! {
    /// What made the player switch source.
    PlaybackSwitchTrigger <=> SwitchTrigger {
        /// The viewer chose another source.
        Manual = "manual",
        /// The player chose it.
        Auto = "auto",
    }
}

// ── The batch a client reports ──────────────────────────────────────────────

/// A playback that started.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackStartEvent {
    /// The source (file) that started. Resolved to its container, codecs and
    /// resolution class, then discarded.
    pub file_id: Uuid,
    /// Which of the source's `audio_tracks` (as `GET /v1/media/{id}/sources`
    /// lists them, from 0) played. Absent, or out of range: the default track,
    /// else the first.
    pub audio_track_index: Option<u32>,
}

/// A playback that failed to start.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackStartFailureEvent {
    /// The source (file) that failed. Resolved, then discarded.
    pub file_id: Uuid,
    pub reason: PlaybackFailureReason,
    pub stage: PlaybackFailureStage,
    /// Which of the source's `audio_tracks` was chosen, as for a start.
    pub audio_track_index: Option<u32>,
}

/// Playback stalled mid-stream waiting for data. A stall before the first
/// frame is part of starting, not a rebuffer.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackRebufferEvent {
    /// The source (file) that was playing. Resolved, then discarded.
    pub file_id: Uuid,
    /// How long playback stalled, 1 ms to 10 minutes.
    #[schema(minimum = 1, maximum = 600000)]
    pub duration_ms: u32,
}

/// The player moved from one source of a title to another.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackSourceSwitchEvent {
    pub from_file_id: Uuid,
    /// Must differ from `from_file_id`.
    pub to_file_id: Uuid,
    pub trigger: PlaybackSwitchTrigger,
}

/// What `POST /v1/telemetry/playback` takes: one client's events since its
/// last report, at most 50 in total. An omitted list is empty.
///
/// A client batches and flushes on its own schedule; the server counts each
/// event under the UTC day it arrives and forgets the file it named. An event
/// whose file the server cannot resolve -- unknown, or missing from disk -- is
/// dropped, not refused.
// kynos gap: `Json<T>` enforces serde only, not the constraints the `Schema`
// derive publishes (`max_items`, `minimum`, `maximum`), although its
// documentation says a schema violation is a 422. The service validates every
// bound below itself (`validate_batch`), answering 422 #validation-failed
// until a kynos release enforces them (upstream issue to be filed against
// getkono/kynos).
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PlaybackTelemetryBatch {
    pub client_kind: PlaybackClientKind,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub starts: Vec<PlaybackStartEvent>,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub start_failures: Vec<PlaybackStartFailureEvent>,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub rebuffers: Vec<PlaybackRebufferEvent>,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub source_switches: Vec<PlaybackSourceSwitchEvent>,
}

impl PlaybackTelemetryBatch {
    /// Every event in the batch, of every kind.
    pub fn event_count(&self) -> usize {
        self.starts.len()
            + self.start_failures.len()
            + self.rebuffers.len()
            + self.source_switches.len()
    }
}

/// One way a request body broke a rule the schema cannot express or the
/// server enforces itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct FieldError {
    /// RFC 6901 JSON Pointer into the request body; empty for the body as a
    /// whole.
    pub pointer: String,
    pub detail: String,
}

/// How a field error rides in a problem document's `errors` extension: as
/// its own serialisation, so the member and the schema above cannot differ.
impl From<FieldError> for serde_json::Value {
    fn from(error: FieldError) -> Self {
        serde_json::to_value(error).expect("two strings always serialise")
    }
}

// ── The report an admin reads ───────────────────────────────────────────────

/// Totals across every row of the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackTelemetryTotals {
    pub started_count: u64,
    pub failed_count: u64,
    pub rebuffer_count: u64,
    /// Summed duration of every rebuffer.
    pub rebuffer_total_ms: u64,
    pub source_switch_count: u64,
}

/// Successful starts under one combination of dimensions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackStartCount {
    pub client_kind: PlaybackClientKind,
    /// Container, lowercased as FFmpeg names it; `unknown` when unnamed.
    pub container: String,
    /// Codec of the first video stream; `none` when there is none.
    pub video_codec: String,
    /// Codec of the audio stream that played; `none` when there is none.
    pub audio_codec: String,
    pub height_class: PlaybackHeightClass,
    pub count: u64,
}

/// Failed starts under one combination of dimensions and one reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackStartFailureCount {
    pub client_kind: PlaybackClientKind,
    pub reason: PlaybackFailureReason,
    pub stage: PlaybackFailureStage,
    pub container: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub height_class: PlaybackHeightClass,
    pub count: u64,
}

/// How many rebuffers fell into one duration range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackRebufferBucketCount {
    pub bucket: PlaybackRebufferBucket,
    pub count: u64,
}

/// Rebuffers under one combination of dimensions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackRebufferCount {
    pub client_kind: PlaybackClientKind,
    pub container: String,
    pub video_codec: String,
    pub height_class: PlaybackHeightClass,
    pub bitrate_class: PlaybackBitrateClass,
    pub event_count: u64,
    pub total_ms: u64,
    /// Every duration range, shortest first, including empty ones.
    pub histogram: Vec<PlaybackRebufferBucketCount>,
}

/// Source switches under one combination of dimensions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackSourceSwitchCount {
    pub client_kind: PlaybackClientKind,
    pub trigger: PlaybackSwitchTrigger,
    pub from_height_class: PlaybackHeightClass,
    pub to_height_class: PlaybackHeightClass,
    pub count: u64,
}

/// What `GET /v1/admin/telemetry/playback` answers: the daily counts from
/// `from` to `to` (UTC days, both inclusive), summed across the days.
///
/// Every list is sorted by its count, largest first, then by its dimensions.
/// No row names a user, a file or a title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Schema)]
pub struct PlaybackTelemetryReport {
    /// Whether `BEAM_PLAYBACK_TELEMETRY_ENABLED` is on -- whether clients'
    /// reports are being counted now. Counts kept while it was on remain
    /// readable after it is turned off.
    pub enabled: bool,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// How many days counts are kept after the day they count.
    pub retention_days: u32,
    pub totals: PlaybackTelemetryTotals,
    pub starts: Vec<PlaybackStartCount>,
    pub start_failures: Vec<PlaybackStartFailureCount>,
    pub rebuffers: Vec<PlaybackRebufferCount>,
    pub source_switches: Vec<PlaybackSourceSwitchCount>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collector or dashboard groups by these strings, and the columns hold
    /// the domain's labels: the wire spelling must be the domain's. Derived
    /// from the domain's own list, not restated.
    #[test]
    fn wire_names_are_the_domain_labels() {
        fn check<D, W>(all: &[D], as_str: fn(D) -> &'static str)
        where
            D: Copy + std::fmt::Debug + PartialEq + From<W>,
            W: From<D> + Serialize,
        {
            for value in all {
                let wire = W::from(*value);
                assert_eq!(
                    serde_json::to_value(&wire).unwrap(),
                    as_str(*value),
                    "{value:?}"
                );
                assert_eq!(D::from(wire), *value, "{value:?} round-trips");
            }
        }
        check::<ClientKind, PlaybackClientKind>(ClientKind::ALL, ClientKind::as_str);
        check::<FailureReason, PlaybackFailureReason>(FailureReason::ALL, FailureReason::as_str);
        check::<FailureStage, PlaybackFailureStage>(FailureStage::ALL, FailureStage::as_str);
        check::<HeightClass, PlaybackHeightClass>(HeightClass::ALL, HeightClass::as_str);
        check::<BitrateClass, PlaybackBitrateClass>(BitrateClass::ALL, BitrateClass::as_str);
        check::<RebufferBucket, PlaybackRebufferBucket>(
            RebufferBucket::ALL,
            RebufferBucket::as_str,
        );
        check::<SwitchTrigger, PlaybackSwitchTrigger>(SwitchTrigger::ALL, SwitchTrigger::as_str);
    }
}
