//! Operator-local playback telemetry (issue #143, ADR-0019): what failed to
//! start, what rebuffered and what switched source, counted per UTC day and
//! per combination of coarse dimensions.
//!
//! Nothing here can name a user, a file or a title. A report arrives naming a
//! file; the server resolves it to the dimensions below -- client kind,
//! container, codecs, a height class and a bitrate class -- and the file id
//! goes no further. That is what keeps a count from being a viewing record.
//!
//! Every vocabulary is a closed enum with one table of stable labels, read by
//! both the SQL repository (the column values) and the wire (the API's enum
//! values), so the two cannot spell a value differently.

/// A dimension recorded when the stream it describes does not exist -- an
/// audio codec for a file with no audio stream. Distinct from
/// [`crate::utils::telemetry::UNKNOWN_LABEL`], a stream that exists but whose
/// codec the prober could not name. `normalize_label` turns a literal `none`
/// into `unknown`, so no codec name can collide with it.
pub const NO_STREAM_LABEL: &str = "none";

/// The label a start's failure reason and stage columns hold when the start
/// succeeded.
pub const NOT_FAILED_LABEL: &str = "none";

/// A closed vocabulary with one stable label per value.
///
/// `ALL` lists every value in declaration order, `as_str` names one, and
/// `parse` reads a name back. The three are generated from one list, so a
/// value cannot be added to one and not the others.
macro_rules! labelled {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $label:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $( $(#[$variant_meta])* $variant ),+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The value's stable label: its column value and its wire name.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            /// The value `label` names, if any.
            pub fn parse(label: &str) -> Option<Self> {
                match label {
                    $($label => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

labelled! {
    /// What kind of client reported. Coarse on purpose: a browser family,
    /// never a version or a user agent.
    pub enum ClientKind {
        WebChromium => "web_chromium",
        WebFirefox => "web_firefox",
        WebSafari => "web_safari",
        WebOther => "web_other",
        Android => "android",
        AndroidTv => "android_tv",
        AppleIos => "apple_ios",
        AppleTvos => "apple_tvos",
        AppleMacos => "apple_macos",
        Other => "other",
    }
}

labelled! {
    /// Why a playback start failed.
    pub enum FailureReason {
        /// The client cannot play the container.
        Container => "container",
        /// The client cannot decode the video stream.
        VideoCodec => "video_codec",
        /// The client cannot decode the chosen audio stream.
        AudioCodec => "audio_codec",
        /// The bytes did not arrive.
        Network => "network",
        /// Anything else.
        Other => "other",
    }
}

labelled! {
    /// Where a playback start failed.
    pub enum FailureStage {
        /// The client ruled the source out before trying it.
        Preflight => "preflight",
        /// The player tried the source and failed.
        Playback => "playback",
    }
}

labelled! {
    /// The vertical resolution class of a video stream.
    pub enum HeightClass {
        /// Under 720 lines.
        Sd => "sd",
        /// 720 up to 1079 lines.
        Hd => "hd",
        /// 1080 up to 2159 lines.
        Fhd => "fhd",
        /// 2160 lines or more.
        Uhd => "uhd",
        /// No video stream, or one whose height the prober did not record.
        Unknown => "unknown",
    }
}

labelled! {
    /// The bitrate class of a whole file: what the network has to carry to
    /// play it at speed.
    pub enum BitrateClass {
        Under2Mbps => "under_2_mbps",
        From2To8Mbps => "from_2_to_8_mbps",
        From8To20Mbps => "from_8_to_20_mbps",
        From20To50Mbps => "from_20_to_50_mbps",
        AtLeast50Mbps => "at_least_50_mbps",
        /// Neither the file's duration nor a stream bitrate was recorded.
        Unknown => "unknown",
    }
}

labelled! {
    /// The duration range one rebuffer falls into.
    pub enum RebufferBucket {
        Under1Secs => "under_1_secs",
        From1To3Secs => "from_1_to_3_secs",
        From3To10Secs => "from_3_to_10_secs",
        From10To30Secs => "from_10_to_30_secs",
        AtLeast30Secs => "at_least_30_secs",
    }
}

labelled! {
    /// What made a player switch to another source of the same title.
    pub enum SwitchTrigger {
        /// The viewer chose another source.
        Manual => "manual",
        /// The player chose it (issue #141).
        Auto => "auto",
    }
}

/// How a playback start ended. A successful start carries no reason and no
/// stage; a failed one always carries both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StartOutcome {
    Started,
    Failed {
        reason: FailureReason,
        stage: FailureStage,
    },
}

impl StartOutcome {
    /// The `outcome` column value.
    pub const fn outcome_label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Failed { .. } => "failed",
        }
    }

    /// The `reason` column value.
    pub const fn reason_label(self) -> &'static str {
        match self {
            Self::Started => NOT_FAILED_LABEL,
            Self::Failed { reason, .. } => reason.as_str(),
        }
    }

    /// The `stage` column value.
    pub const fn stage_label(self) -> &'static str {
        match self {
            Self::Started => NOT_FAILED_LABEL,
            Self::Failed { stage, .. } => stage.as_str(),
        }
    }

    /// The outcome three column values describe, if they describe one: a
    /// `started` row with a reason or stage, or a `failed` row without them,
    /// describes nothing.
    pub fn from_labels(outcome: &str, reason: &str, stage: &str) -> Option<Self> {
        match outcome {
            "started" if reason == NOT_FAILED_LABEL && stage == NOT_FAILED_LABEL => {
                Some(Self::Started)
            }
            "failed" => Some(Self::Failed {
                reason: FailureReason::parse(reason)?,
                stage: FailureStage::parse(stage)?,
            }),
            _ => None,
        }
    }
}

/// The dimensions one playback start is counted under.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StartKey {
    pub client_kind: ClientKind,
    pub outcome: StartOutcome,
    /// Normalised container label.
    pub container: String,
    /// Normalised codec label of the first video stream, or
    /// [`NO_STREAM_LABEL`].
    pub video_codec: String,
    /// Normalised codec label of the audio stream the client chose, or
    /// [`NO_STREAM_LABEL`].
    pub audio_codec: String,
    pub height_class: HeightClass,
}

/// The dimensions one rebuffer is counted under.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RebufferKey {
    pub client_kind: ClientKind,
    pub container: String,
    pub video_codec: String,
    pub height_class: HeightClass,
    pub bitrate_class: BitrateClass,
}

/// The dimensions one source switch is counted under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SwitchKey {
    pub client_kind: ClientKind,
    pub trigger: SwitchTrigger,
    pub from_height_class: HeightClass,
    pub to_height_class: HeightClass,
}

/// Rebuffer counts per [`RebufferBucket`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebufferHistogram {
    counts: [u64; RebufferBucket::ALL.len()],
}

impl RebufferHistogram {
    /// A histogram from one count per bucket, in [`RebufferBucket::ALL`]
    /// order.
    pub const fn from_counts(counts: [u64; RebufferBucket::ALL.len()]) -> Self {
        Self { counts }
    }

    /// Counts one rebuffer in `bucket`.
    pub fn record(&mut self, bucket: RebufferBucket) {
        self.counts[bucket as usize] += 1;
    }

    /// Adds every count of `other` to this one.
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.counts.iter_mut().zip(other.counts) {
            *mine += theirs;
        }
    }

    pub fn count(&self, bucket: RebufferBucket) -> u64 {
        self.counts[bucket as usize]
    }

    /// Every bucket and its count, shortest first.
    pub fn iter(&self) -> impl Iterator<Item = (RebufferBucket, u64)> + '_ {
        RebufferBucket::ALL.iter().copied().zip(self.counts)
    }
}

/// How many starts were counted under one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartCount {
    pub key: StartKey,
    pub count: u64,
}

/// The rebuffers counted under one key: how many, how long in total, and how
/// they fall across the duration ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebufferCount {
    pub key: RebufferKey,
    pub events: u64,
    pub total_ms: u64,
    pub histogram: RebufferHistogram,
}

/// How many source switches were counted under one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchCount {
    pub key: SwitchKey,
    pub count: u64,
}

/// What [`crate::repositories::PlaybackTelemetryRepository::summarize`]
/// answers: every counter in a range of days, summed across the days, one
/// entry per key, each list sorted by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackTelemetrySummary {
    pub starts: Vec<StartCount>,
    pub rebuffers: Vec<RebufferCount>,
    pub switches: Vec<SwitchCount>,
}

/// One event to count, already resolved to the key it is counted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackTelemetryEvent {
    /// A playback start, successful or failed.
    Start(StartKey),
    /// A rebuffer lasting `duration_ms`.
    Rebuffer { key: RebufferKey, duration_ms: u32 },
    /// A source switch.
    Switch(SwitchKey),
}

impl PlaybackTelemetrySummary {
    /// What counting `events` adds: one entry per distinct key, each list
    /// sorted by key, exactly as [`crate::repositories::PlaybackTelemetryRepository::summarize`]
    /// would read the events back from an empty store. A batch is written as
    /// its tally, so a key named twice is one increment of two, and every
    /// writer takes its rows in the same order.
    pub fn tally(events: &[PlaybackTelemetryEvent]) -> Self {
        use std::collections::BTreeMap;

        let mut starts: BTreeMap<&StartKey, u64> = BTreeMap::new();
        let mut rebuffers: BTreeMap<&RebufferKey, (u64, u64, RebufferHistogram)> = BTreeMap::new();
        let mut switches: BTreeMap<SwitchKey, u64> = BTreeMap::new();
        for event in events {
            match event {
                PlaybackTelemetryEvent::Start(key) => *starts.entry(key).or_insert(0) += 1,
                PlaybackTelemetryEvent::Rebuffer { key, duration_ms } => {
                    let (events, total_ms, histogram) = rebuffers.entry(key).or_default();
                    *events += 1;
                    *total_ms += u64::from(*duration_ms);
                    histogram.record(crate::utils::telemetry::rebuffer_bucket(*duration_ms));
                }
                PlaybackTelemetryEvent::Switch(key) => *switches.entry(*key).or_insert(0) += 1,
            }
        }

        Self {
            starts: starts
                .into_iter()
                .map(|(key, count)| StartCount {
                    key: key.clone(),
                    count,
                })
                .collect(),
            rebuffers: rebuffers
                .into_iter()
                .map(|(key, (events, total_ms, histogram))| RebufferCount {
                    key: key.clone(),
                    events,
                    total_ms,
                    histogram,
                })
                .collect(),
            switches: switches
                .into_iter()
                .map(|(key, count)| SwitchCount { key, count })
                .collect(),
        }
    }

    /// Whether the summary counts nothing at all.
    pub fn is_empty(&self) -> bool {
        self.starts.is_empty() && self.rebuffers.is_empty() && self.switches.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::test_utils::{rebuffer_key, start_key, switch_key};
    use super::*;

    /// Checks one vocabulary: every label reads back as its value, no two
    /// values share a label, and every label is snake_case -- the column
    /// values and wire names both rely on all three.
    fn check_vocabulary<T: Copy + Eq + std::fmt::Debug>(
        all: &[T],
        as_str: fn(T) -> &'static str,
        parse: fn(&str) -> Option<T>,
    ) {
        let mut seen = HashSet::new();
        for value in all {
            let label = as_str(*value);
            assert_eq!(parse(label), Some(*value), "{label}");
            assert!(seen.insert(label), "{label} is used twice");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && label.starts_with(|c: char| c.is_ascii_lowercase()),
                "{label} is not snake_case"
            );
        }
        assert_eq!(parse("not a label"), None);
    }

    #[test]
    fn every_vocabulary_reads_back_its_own_labels() {
        check_vocabulary(ClientKind::ALL, ClientKind::as_str, ClientKind::parse);
        check_vocabulary(
            FailureReason::ALL,
            FailureReason::as_str,
            FailureReason::parse,
        );
        check_vocabulary(FailureStage::ALL, FailureStage::as_str, FailureStage::parse);
        check_vocabulary(HeightClass::ALL, HeightClass::as_str, HeightClass::parse);
        check_vocabulary(BitrateClass::ALL, BitrateClass::as_str, BitrateClass::parse);
        check_vocabulary(
            RebufferBucket::ALL,
            RebufferBucket::as_str,
            RebufferBucket::parse,
        );
        check_vocabulary(
            SwitchTrigger::ALL,
            SwitchTrigger::as_str,
            SwitchTrigger::parse,
        );
    }

    /// Every outcome reads back from its own three labels.
    #[test]
    fn every_outcome_reads_back_from_its_labels() {
        let mut outcomes = vec![StartOutcome::Started];
        for reason in FailureReason::ALL {
            for stage in FailureStage::ALL {
                outcomes.push(StartOutcome::Failed {
                    reason: *reason,
                    stage: *stage,
                });
            }
        }
        for outcome in outcomes {
            assert_eq!(
                StartOutcome::from_labels(
                    outcome.outcome_label(),
                    outcome.reason_label(),
                    outcome.stage_label()
                ),
                Some(outcome)
            );
        }
    }

    /// Labels that do not describe one outcome -- the rows the table's
    /// `CHECK` refuses -- read as none rather than as a guess.
    #[test]
    fn incoherent_outcome_labels_read_as_none() {
        let cases = [
            ("started", "network", NOT_FAILED_LABEL),
            ("started", NOT_FAILED_LABEL, "preflight"),
            ("failed", NOT_FAILED_LABEL, "preflight"),
            ("failed", "network", NOT_FAILED_LABEL),
            ("failed", "bogus", "preflight"),
            ("aborted", NOT_FAILED_LABEL, NOT_FAILED_LABEL),
        ];
        for (outcome, reason, stage) in cases {
            assert_eq!(
                StartOutcome::from_labels(outcome, reason, stage),
                None,
                "{outcome}/{reason}/{stage}"
            );
        }
    }

    /// A key named more than once is one entry with its events summed; keys
    /// differing in a dimension stay apart; each list comes out sorted.
    #[test]
    fn a_tally_folds_repeated_keys_and_sorts_each_list() {
        let hd = StartKey {
            height_class: HeightClass::Hd,
            ..start_key()
        };
        let auto = SwitchKey {
            trigger: SwitchTrigger::Auto,
            ..switch_key()
        };
        let events = vec![
            PlaybackTelemetryEvent::Switch(auto),
            PlaybackTelemetryEvent::Start(start_key()),
            PlaybackTelemetryEvent::Rebuffer {
                key: rebuffer_key(),
                duration_ms: 999,
            },
            PlaybackTelemetryEvent::Start(hd.clone()),
            PlaybackTelemetryEvent::Switch(switch_key()),
            PlaybackTelemetryEvent::Start(start_key()),
            PlaybackTelemetryEvent::Rebuffer {
                key: rebuffer_key(),
                duration_ms: 30_000,
            },
            PlaybackTelemetryEvent::Switch(auto),
        ];

        let tally = PlaybackTelemetrySummary::tally(&events);

        let mut starts = vec![(start_key(), 2), (hd, 1)];
        starts.sort();
        let read: Vec<(StartKey, u64)> = tally
            .starts
            .iter()
            .map(|s| (s.key.clone(), s.count))
            .collect();
        assert_eq!(read, starts);
        let mut histogram = RebufferHistogram::default();
        histogram.record(RebufferBucket::Under1Secs);
        histogram.record(RebufferBucket::AtLeast30Secs);
        assert_eq!(
            tally.rebuffers,
            vec![RebufferCount {
                key: rebuffer_key(),
                events: 2,
                total_ms: 30_999,
                histogram,
            }]
        );
        let mut switches = vec![(switch_key(), 1), (auto, 2)];
        switches.sort();
        let read: Vec<(SwitchKey, u64)> = tally.switches.iter().map(|s| (s.key, s.count)).collect();
        assert_eq!(read, switches);
        assert!(!tally.is_empty());
        assert!(PlaybackTelemetrySummary::tally(&[]).is_empty());
    }

    fn event() -> impl Strategy<Value = PlaybackTelemetryEvent> {
        let height = prop::sample::select(HeightClass::ALL);
        prop_oneof![
            height
                .clone()
                .prop_map(|height_class| PlaybackTelemetryEvent::Start(StartKey {
                    height_class,
                    ..start_key()
                })),
            (height.clone(), 1u32..=600_000).prop_map(|(height_class, duration_ms)| {
                PlaybackTelemetryEvent::Rebuffer {
                    key: RebufferKey {
                        height_class,
                        ..rebuffer_key()
                    },
                    duration_ms,
                }
            }),
            height.prop_map(|to_height_class| PlaybackTelemetryEvent::Switch(SwitchKey {
                to_height_class,
                ..switch_key()
            })),
        ]
    }

    proptest! {
        /// A tally loses and invents nothing: every event is counted once,
        /// every rebuffer's duration once, every rebuffer in one bucket, and
        /// no key appears twice.
        #[test]
        fn a_tally_counts_every_event_exactly_once(events in prop::collection::vec(event(), 0..60)) {
            let tally = PlaybackTelemetrySummary::tally(&events);

            let mut starts = 0u64;
            let mut rebuffers = 0u64;
            let mut total_ms = 0u64;
            let mut switches = 0u64;
            for event in &events {
                match event {
                    PlaybackTelemetryEvent::Start(_) => starts += 1,
                    PlaybackTelemetryEvent::Rebuffer { duration_ms, .. } => {
                        rebuffers += 1;
                        total_ms += u64::from(*duration_ms);
                    }
                    PlaybackTelemetryEvent::Switch(_) => switches += 1,
                }
            }
            prop_assert_eq!(tally.starts.iter().map(|s| s.count).sum::<u64>(), starts);
            prop_assert_eq!(tally.rebuffers.iter().map(|r| r.events).sum::<u64>(), rebuffers);
            prop_assert_eq!(tally.rebuffers.iter().map(|r| r.total_ms).sum::<u64>(), total_ms);
            for row in &tally.rebuffers {
                prop_assert_eq!(row.histogram.iter().map(|(_, n)| n).sum::<u64>(), row.events);
            }
            prop_assert_eq!(tally.switches.iter().map(|s| s.count).sum::<u64>(), switches);
            prop_assert!(tally.starts.windows(2).all(|w| w[0].key < w[1].key));
            prop_assert!(tally.rebuffers.windows(2).all(|w| w[0].key < w[1].key));
            prop_assert!(tally.switches.windows(2).all(|w| w[0].key < w[1].key));
            prop_assert_eq!(tally.is_empty(), events.is_empty());
        }
    }
}

/// Test data builders: a plausible key of each kind, for a test to adjust
/// with struct-update syntax rather than spell out every dimension.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::*;

    /// A successful start on a Chromium browser of a 1080p H.264/AAC
    /// Matroska file.
    pub fn start_key() -> StartKey {
        StartKey {
            client_kind: ClientKind::WebChromium,
            outcome: StartOutcome::Started,
            container: "matroska,webm".to_string(),
            video_codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            height_class: HeightClass::Fhd,
        }
    }

    /// A rebuffer on a Chromium browser of a 1080p H.264 Matroska file.
    pub fn rebuffer_key() -> RebufferKey {
        RebufferKey {
            client_kind: ClientKind::WebChromium,
            container: "matroska,webm".to_string(),
            video_codec: "h264".to_string(),
            height_class: HeightClass::Fhd,
            bitrate_class: BitrateClass::From8To20Mbps,
        }
    }

    /// A manual switch on a Chromium browser from 2160p to 1080p.
    pub fn switch_key() -> SwitchKey {
        SwitchKey {
            client_kind: ClientKind::WebChromium,
            trigger: SwitchTrigger::Manual,
            from_height_class: HeightClass::Uhd,
            to_height_class: HeightClass::Fhd,
        }
    }
}
