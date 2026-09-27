//! The coarsening the anonymous library report applies (issue #93, ADR-0019).
//!
//! Everything here decides how much a report *cannot* say. No number is
//! reported exactly -- a count falls into one of a few orders of magnitude
//! ([`CountBucket`]), a file's size into one of a few [`FileSizeBucket`]s, and
//! the library's total into one of a few [`TotalSizeBucket`]s -- and every
//! free-form label (a container or codec name) passes through
//! [`normalize_label`] before it can leave the process.
//!
//! Pure functions and constants only: the SQL repository generates its
//! histogram from [`FileSizeBucket::ALL`], and the in-memory double buckets
//! with [`FileSizeBucket::of`], so the two cannot disagree on a boundary.
//!
//! The playback telemetry classes (issue #143) live here too: the height,
//! bitrate and rebuffer-duration ranges a playback counter is kept under.

use crate::models::playback_telemetry::{BitrateClass, HeightClass, RebufferBucket};

/// One gibibyte.
pub const GIB: u64 = 1 << 30;

/// One tebibyte.
pub const TIB: u64 = 1 << 40;

/// The label a missing or unusable container/codec name is reported under.
pub const UNKNOWN_LABEL: &str = "unknown";

/// The longest label a report carries. FFmpeg's container names are the
/// longest legitimate values (`mov,mp4,m4a,3gp,3g2,mj2` is 23 characters);
/// anything past this is not a name FFmpeg produced.
pub const MAX_LABEL_LEN: usize = 48;

/// The order of magnitude a count falls into -- how a report says how many of
/// anything there are.
///
/// A report carries no exact count: `4_812` episodes and `4_813` read alike,
/// so a week's additions do not make one server's reports trivially line up
/// with each other (NFR-503). Each bucket covers `[lower_bound, next bucket's
/// lower bound)`; the last is open-ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CountBucket {
    None,
    From1To9,
    From10To99,
    From100To999,
    From1000To9999,
    AtLeast10000,
}

impl CountBucket {
    /// The one table the buckets are read from: each bucket in declaration
    /// order, the smallest count it holds, and its stable wire name.
    const TABLE: [(Self, u64, &'static str); 6] = [
        (Self::None, 0, "none"),
        (Self::From1To9, 1, "from_1_to_9"),
        (Self::From10To99, 10, "from_10_to_99"),
        (Self::From100To999, 100, "from_100_to_999"),
        (Self::From1000To9999, 1_000, "from_1000_to_9999"),
        (Self::AtLeast10000, 10_000, "at_least_10000"),
    ];

    /// Every bucket, smallest first.
    pub const ALL: [Self; Self::TABLE.len()] = {
        let mut all = [Self::None; Self::TABLE.len()];
        let mut index = 0;
        while index < all.len() {
            all[index] = Self::TABLE[index].0;
            index += 1;
        }
        all
    };

    /// The smallest count this bucket holds.
    pub const fn lower_bound(self) -> u64 {
        Self::TABLE[self as usize].1
    }

    /// The first count past this bucket; `None` for the last.
    pub fn upper_bound(self) -> Option<u64> {
        Self::TABLE
            .get(self as usize + 1)
            .map(|(_, lower_bound, _)| *lower_bound)
    }

    /// The bucket `count` falls into.
    pub fn of(count: u64) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|bucket| count >= bucket.lower_bound())
            .unwrap_or(Self::None)
    }

    /// The bucket's stable wire name.
    pub const fn as_str(self) -> &'static str {
        Self::TABLE[self as usize].2
    }
}

/// The size range one file falls into.
///
/// Ordered by lower bound: each bucket covers `[lower_bound_bytes, next
/// bucket's lower bound)`, and the last is open-ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileSizeBucket {
    Under1Gib,
    From1To4Gib,
    From4To10Gib,
    From10To25Gib,
    From25To50Gib,
    AtLeast50Gib,
}

impl FileSizeBucket {
    /// Every bucket, smallest first. The SQL histogram and the report's
    /// field list are both generated from this.
    pub const ALL: [Self; 6] = [
        Self::Under1Gib,
        Self::From1To4Gib,
        Self::From4To10Gib,
        Self::From10To25Gib,
        Self::From25To50Gib,
        Self::AtLeast50Gib,
    ];

    /// The smallest size, in bytes, this bucket holds.
    pub const fn lower_bound_bytes(self) -> u64 {
        match self {
            Self::Under1Gib => 0,
            Self::From1To4Gib => GIB,
            Self::From4To10Gib => 4 * GIB,
            Self::From10To25Gib => 10 * GIB,
            Self::From25To50Gib => 25 * GIB,
            Self::AtLeast50Gib => 50 * GIB,
        }
    }

    /// The first size, in bytes, past this bucket; `None` for the last.
    pub fn upper_bound_bytes(self) -> Option<u64> {
        let index = Self::ALL.iter().position(|b| *b == self)?;
        Self::ALL
            .get(index + 1)
            .map(|next| next.lower_bound_bytes())
    }

    /// The bucket a file of `size_bytes` falls into.
    pub fn of(size_bytes: u64) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|bucket| size_bytes >= bucket.lower_bound_bytes())
            .unwrap_or(Self::Under1Gib)
    }

    /// The bucket's stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Under1Gib => "under_1_gib",
            Self::From1To4Gib => "from_1_to_4_gib",
            Self::From4To10Gib => "from_4_to_10_gib",
            Self::From10To25Gib => "from_10_to_25_gib",
            Self::From25To50Gib => "from_25_to_50_gib",
            Self::AtLeast50Gib => "at_least_50_gib",
        }
    }
}

/// How many files fall into each [`FileSizeBucket`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileSizeHistogram {
    counts: [u64; FileSizeBucket::ALL.len()],
}

impl FileSizeHistogram {
    /// A histogram from per-bucket counts, in [`FileSizeBucket::ALL`] order.
    pub const fn from_counts(counts: [u64; FileSizeBucket::ALL.len()]) -> Self {
        Self { counts }
    }

    /// Counts one file of `size_bytes`.
    pub fn record(&mut self, size_bytes: u64) {
        self.counts[FileSizeBucket::of(size_bytes) as usize] += 1;
    }

    /// Files in `bucket`.
    pub fn count(&self, bucket: FileSizeBucket) -> u64 {
        self.counts[bucket as usize]
    }

    /// Every bucket with its count, smallest first.
    pub fn iter(&self) -> impl Iterator<Item = (FileSizeBucket, u64)> + '_ {
        FileSizeBucket::ALL
            .into_iter()
            .map(|bucket| (bucket, self.count(bucket)))
    }

    /// Files across every bucket.
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

/// The range a whole server's indexed bytes fall into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TotalSizeBucket {
    Under100Gib,
    From100GibTo1Tib,
    From1To10Tib,
    From10To50Tib,
    AtLeast50Tib,
}

impl TotalSizeBucket {
    /// Every bucket, smallest first.
    pub const ALL: [Self; 5] = [
        Self::Under100Gib,
        Self::From100GibTo1Tib,
        Self::From1To10Tib,
        Self::From10To50Tib,
        Self::AtLeast50Tib,
    ];

    /// The smallest total, in bytes, this bucket holds.
    pub const fn lower_bound_bytes(self) -> u64 {
        match self {
            Self::Under100Gib => 0,
            Self::From100GibTo1Tib => 100 * GIB,
            Self::From1To10Tib => TIB,
            Self::From10To50Tib => 10 * TIB,
            Self::AtLeast50Tib => 50 * TIB,
        }
    }

    /// The bucket a total of `total_bytes` falls into.
    pub fn of(total_bytes: u64) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|bucket| total_bytes >= bucket.lower_bound_bytes())
            .unwrap_or(Self::Under100Gib)
    }

    /// The bucket's stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Under100Gib => "under_100_gib",
            Self::From100GibTo1Tib => "from_100_gib_to_1_tib",
            Self::From1To10Tib => "from_1_to_10_tib",
            Self::From10To50Tib => "from_10_to_50_tib",
            Self::AtLeast50Tib => "at_least_50_tib",
        }
    }
}

/// A container or codec name as a report may carry it.
///
/// Lowercased, so `H264` (the indexer's current `Debug` spelling of a codec
/// id) and `h264` (FFmpeg's own name) are one label and the payload does not
/// change shape when the indexer's spelling does. An `Other("x")` wrapper --
/// the `Debug` form of a codec the prober does not name -- is unwrapped to
/// `x`. Anything outside `[a-z0-9_.,+-]` is dropped, which is what keeps a
/// path separator, whitespace or quote out of a label whatever arrives; an
/// empty, `none`, or overlong result is [`UNKNOWN_LABEL`].
pub fn normalize_label(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    let unwrapped = lowered
        .strip_prefix("other(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(&lowered);
    let label: String = unwrapped
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.,+-".contains(*c))
        .collect();
    if label.is_empty() || label == "none" || label.len() > MAX_LABEL_LEN {
        UNKNOWN_LABEL.to_string()
    } else {
        label
    }
}

// ── Playback telemetry classes (issue #143) ─────────────────────────────────
//
// The coarse dimensions a playback counter is kept under. Each class is read
// from one table of lower bounds, so a boundary lives in exactly one place.

/// The fewest lines each [`HeightClass`] holds, ascending. A height of zero --
/// the prober recorded none -- is below all of them.
const HEIGHT_LOWER_BOUNDS: [(HeightClass, u32); 4] = [
    (HeightClass::Sd, 1),
    (HeightClass::Hd, 720),
    (HeightClass::Fhd, 1080),
    (HeightClass::Uhd, 2160),
];

/// The class a video stream `height` lines tall falls into; `Unknown` for a
/// height the prober did not record (zero).
pub fn height_class(height: u32) -> HeightClass {
    HEIGHT_LOWER_BOUNDS
        .iter()
        .rev()
        .find(|(_, lower)| height >= *lower)
        .map_or(HeightClass::Unknown, |(class, _)| *class)
}

/// The lowest bitrate, in bits per second, each known [`BitrateClass`] holds,
/// ascending.
const BITRATE_LOWER_BOUNDS: [(BitrateClass, u64); 5] = [
    (BitrateClass::Under2Mbps, 0),
    (BitrateClass::From2To8Mbps, 2_000_000),
    (BitrateClass::From8To20Mbps, 8_000_000),
    (BitrateClass::From20To50Mbps, 20_000_000),
    (BitrateClass::AtLeast50Mbps, 50_000_000),
];

/// The class a bitrate of `bits_per_sec` falls into; `Unknown` when there is
/// none to classify.
pub fn bitrate_class(bits_per_sec: Option<u64>) -> BitrateClass {
    let Some(rate) = bits_per_sec else {
        return BitrateClass::Unknown;
    };
    BITRATE_LOWER_BOUNDS
        .iter()
        .rev()
        .find(|(_, lower)| rate >= *lower)
        .map_or(BitrateClass::Unknown, |(class, _)| *class)
}

/// A whole file's average bitrate in bits per second: its size over its
/// duration. `None` when the duration is unknown or zero.
pub fn file_bitrate(size_bytes: u64, duration: Option<std::time::Duration>) -> Option<u64> {
    let secs = duration?.as_secs_f64();
    if secs <= 0.0 {
        return None;
    }
    // Saturating: an absurd size over a tiny duration is simply the top class.
    Some((size_bytes as f64 * 8.0 / secs).min(u64::MAX as f64) as u64)
}

/// The shortest duration, in milliseconds, each [`RebufferBucket`] holds,
/// ascending.
const REBUFFER_LOWER_BOUNDS_MS: [(RebufferBucket, u32); 5] = [
    (RebufferBucket::Under1Secs, 0),
    (RebufferBucket::From1To3Secs, 1_000),
    (RebufferBucket::From3To10Secs, 3_000),
    (RebufferBucket::From10To30Secs, 10_000),
    (RebufferBucket::AtLeast30Secs, 30_000),
];

/// The range a rebuffer lasting `duration_ms` falls into.
pub fn rebuffer_bucket(duration_ms: u32) -> RebufferBucket {
    REBUFFER_LOWER_BOUNDS_MS
        .iter()
        .rev()
        .find(|(_, lower)| duration_ms >= *lower)
        .map_or(RebufferBucket::Under1Secs, |(bucket, _)| *bucket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Each boundary belongs to the bucket above it, and one byte short
    /// belongs to the bucket below -- walked over every bucket rather than
    /// restated, so a bound edited in one place is still checked.
    #[test]
    fn every_file_size_boundary_opens_its_bucket() {
        for window in FileSizeBucket::ALL.windows(2) {
            let (below, above) = (window[0], window[1]);
            let bound = above.lower_bound_bytes();
            assert_eq!(FileSizeBucket::of(bound), above, "{bound} opens {above:?}");
            assert_eq!(
                FileSizeBucket::of(bound - 1),
                below,
                "{bound} - 1 is still {below:?}"
            );
            assert_eq!(below.upper_bound_bytes(), Some(bound));
        }
        assert_eq!(FileSizeBucket::of(0), FileSizeBucket::Under1Gib);
        assert_eq!(FileSizeBucket::of(u64::MAX), FileSizeBucket::AtLeast50Gib);
        assert_eq!(FileSizeBucket::AtLeast50Gib.upper_bound_bytes(), None);
    }

    /// The boundaries the issue names, pinned once: a 1 GiB file is not
    /// "under 1 GiB".
    #[test]
    fn a_gibibyte_file_is_not_under_a_gibibyte() {
        assert_eq!(FileSizeBucket::of(GIB - 1), FileSizeBucket::Under1Gib);
        assert_eq!(FileSizeBucket::of(GIB), FileSizeBucket::From1To4Gib);
    }

    #[test]
    fn every_total_size_boundary_opens_its_bucket() {
        for window in TotalSizeBucket::ALL.windows(2) {
            let (below, above) = (window[0], window[1]);
            let bound = above.lower_bound_bytes();
            assert_eq!(TotalSizeBucket::of(bound), above);
            assert_eq!(TotalSizeBucket::of(bound - 1), below);
        }
        assert_eq!(TotalSizeBucket::of(0), TotalSizeBucket::Under100Gib);
        assert_eq!(TotalSizeBucket::of(u64::MAX), TotalSizeBucket::AtLeast50Tib);
    }

    /// The table is indexed by discriminant, so its rows must be in
    /// declaration order -- a row out of place would give a bucket another
    /// bucket's bound and name.
    #[test]
    fn the_count_table_is_in_declaration_order() {
        for (index, bucket) in CountBucket::ALL.into_iter().enumerate() {
            assert_eq!(bucket as usize, index, "{bucket:?}");
        }
    }

    #[test]
    fn every_count_boundary_opens_its_bucket() {
        for window in CountBucket::ALL.windows(2) {
            let (below, above) = (window[0], window[1]);
            let bound = above.lower_bound();
            assert!(bound > below.lower_bound(), "{above:?} is above {below:?}");
            assert_eq!(CountBucket::of(bound), above, "{bound} opens {above:?}");
            assert_eq!(
                CountBucket::of(bound - 1),
                below,
                "{bound} - 1 is still {below:?}"
            );
            assert_eq!(below.upper_bound(), Some(bound));
        }
        assert_eq!(
            CountBucket::ALL[0].lower_bound(),
            0,
            "every count has a bucket"
        );
        assert_eq!(CountBucket::of(u64::MAX), CountBucket::AtLeast10000);
        assert_eq!(CountBucket::AtLeast10000.upper_bound(), None);
    }

    /// Nothing and something are never one bucket: a report says whether a
    /// codec occurs at all, whatever else it coarsens.
    #[test]
    fn zero_is_a_bucket_of_its_own() {
        assert_eq!(CountBucket::ALL[0].upper_bound(), Some(1));
        assert_ne!(CountBucket::of(0), CountBucket::of(1));
    }

    /// The wire name says what the bucket holds, so a collector reading
    /// `from_10_to_99` knows the bounds without this source. Derived from
    /// the bounds, not restated.
    #[test]
    fn each_count_name_spells_its_bounds() {
        for bucket in CountBucket::ALL {
            let lower = bucket.lower_bound();
            let expected = match bucket.upper_bound() {
                Some(1) => "none".to_string(),
                Some(upper) => format!("from_{lower}_to_{}", upper - 1),
                None => format!("at_least_{lower}"),
            };
            assert_eq!(bucket.as_str(), expected);
        }
    }

    /// Wire names are what a collector groups by, so two buckets sharing one
    /// would silently merge their counts.
    #[test]
    fn bucket_names_are_distinct_snake_case() {
        let names: Vec<&str> = FileSizeBucket::ALL
            .iter()
            .map(|b| b.as_str())
            .chain(TotalSizeBucket::ALL.iter().map(|b| b.as_str()))
            .chain(CountBucket::ALL.iter().map(|b| b.as_str()))
            .collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate bucket name: {names:?}"
        );
        for name in names {
            assert!(
                name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{name} is not snake_case"
            );
        }
    }

    #[test]
    fn labels_are_lowercased_and_unwrapped() {
        for (raw, expected) in [
            ("H264", "h264"),
            ("h264", "h264"),
            ("SUBRIP", "subrip"),
            ("Other(\"hdmv_pgs_subtitle\")", "hdmv_pgs_subtitle"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "mov,mp4,m4a,3gp,3g2,mj2"),
            ("  matroska,webm ", "matroska,webm"),
            ("None", UNKNOWN_LABEL),
            ("", UNKNOWN_LABEL),
            ("\"\"", UNKNOWN_LABEL),
        ] {
            assert_eq!(normalize_label(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn an_overlong_label_is_unknown() {
        assert_eq!(
            normalize_label(&"a".repeat(MAX_LABEL_LEN)),
            "a".repeat(MAX_LABEL_LEN)
        );
        assert_eq!(
            normalize_label(&"a".repeat(MAX_LABEL_LEN + 1)),
            UNKNOWN_LABEL
        );
    }

    proptest! {
        /// Every count lands in the one bucket whose bounds hold it.
        #[test]
        fn a_count_is_inside_its_bucket(count in any::<u64>()) {
            let bucket = CountBucket::of(count);
            prop_assert!(bucket.lower_bound() <= count);
            prop_assert!(bucket.upper_bound().is_none_or(|upper| count < upper));
        }

        /// Whatever reaches the report, no path separator, whitespace or
        /// quote survives, and the result is never empty.
        #[test]
        fn a_normalized_label_carries_no_path_or_whitespace(raw in ".*") {
            let label = normalize_label(&raw);
            prop_assert!(!label.is_empty());
            prop_assert!(label.len() <= MAX_LABEL_LEN);
            prop_assert!(!label.contains(['/', '\\', ' ', '"', '\'']));
            prop_assert!(!label.chars().any(|c| c.is_ascii_uppercase() || c.is_whitespace()));
        }

        /// Normalising twice changes nothing, so re-aggregating already
        /// normalised labels cannot split one bucket in two.
        #[test]
        fn normalization_is_idempotent(raw in ".*") {
            let once = normalize_label(&raw);
            prop_assert_eq!(normalize_label(&once), once);
        }

        /// Every file lands in exactly one bucket, so a histogram always
        /// accounts for every file recorded into it.
        #[test]
        fn a_histogram_accounts_for_every_file(sizes in proptest::collection::vec(any::<u64>(), 0..64)) {
            let mut histogram = FileSizeHistogram::default();
            for size in &sizes {
                histogram.record(*size);
            }
            prop_assert_eq!(histogram.total(), sizes.len() as u64);
            for (bucket, count) in histogram.iter() {
                let expected = sizes.iter().filter(|s| FileSizeBucket::of(**s) == bucket).count() as u64;
                prop_assert_eq!(count, expected);
            }
        }
    }
}

#[cfg(test)]
mod playback_class_tests {
    use std::time::Duration;

    use super::*;
    use proptest::prelude::*;

    /// The resolution boundaries issue #143 names: each opens its class and
    /// one line short is still the class below.
    #[test]
    fn height_boundaries_open_their_class() {
        let cases = [
            (0, HeightClass::Unknown),
            (1, HeightClass::Sd),
            (719, HeightClass::Sd),
            (720, HeightClass::Hd),
            (1079, HeightClass::Hd),
            (1080, HeightClass::Fhd),
            (2159, HeightClass::Fhd),
            (2160, HeightClass::Uhd),
            (u32::MAX, HeightClass::Uhd),
        ];
        for (height, expected) in cases {
            assert_eq!(height_class(height), expected, "{height} lines");
        }
    }

    #[test]
    fn bitrate_boundaries_open_their_class() {
        let cases = [
            (None, BitrateClass::Unknown),
            (Some(0), BitrateClass::Under2Mbps),
            (Some(1_999_999), BitrateClass::Under2Mbps),
            (Some(2_000_000), BitrateClass::From2To8Mbps),
            (Some(7_999_999), BitrateClass::From2To8Mbps),
            (Some(8_000_000), BitrateClass::From8To20Mbps),
            (Some(19_999_999), BitrateClass::From8To20Mbps),
            (Some(20_000_000), BitrateClass::From20To50Mbps),
            (Some(49_999_999), BitrateClass::From20To50Mbps),
            (Some(50_000_000), BitrateClass::AtLeast50Mbps),
            (Some(u64::MAX), BitrateClass::AtLeast50Mbps),
        ];
        for (rate, expected) in cases {
            assert_eq!(bitrate_class(rate), expected, "{rate:?} b/s");
        }
    }

    #[test]
    fn rebuffer_boundaries_open_their_bucket() {
        let cases = [
            (0, RebufferBucket::Under1Secs),
            (999, RebufferBucket::Under1Secs),
            (1_000, RebufferBucket::From1To3Secs),
            (2_999, RebufferBucket::From1To3Secs),
            (3_000, RebufferBucket::From3To10Secs),
            (9_999, RebufferBucket::From3To10Secs),
            (10_000, RebufferBucket::From10To30Secs),
            (29_999, RebufferBucket::From10To30Secs),
            (30_000, RebufferBucket::AtLeast30Secs),
            (u32::MAX, RebufferBucket::AtLeast30Secs),
        ];
        for (duration_ms, expected) in cases {
            assert_eq!(rebuffer_bucket(duration_ms), expected, "{duration_ms} ms");
        }
    }

    #[test]
    fn a_file_bitrate_is_its_size_over_its_duration() {
        // 1 GB over 1000 s is 8 Mb/s.
        assert_eq!(
            file_bitrate(1_000_000_000, Some(Duration::from_secs(1_000))),
            Some(8_000_000)
        );
        assert_eq!(file_bitrate(1_000_000_000, None), None);
        assert_eq!(file_bitrate(1_000_000_000, Some(Duration::ZERO)), None);
    }

    proptest! {
        /// Heights only ever climb the classes: a taller stream is never in a
        /// lower class, so the report's ordering of classes is meaningful.
        #[test]
        fn height_classes_are_monotonic(a in 1u32.., b in 1u32..) {
            let (low, high) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(height_class(low) <= height_class(high));
        }

        #[test]
        fn rebuffer_buckets_are_monotonic(a: u32, b: u32) {
            let (low, high) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(rebuffer_bucket(low) <= rebuffer_bucket(high));
        }
    }
}
