//! The coarsening the anonymous library report applies (issue #93, ADR-0019).
//!
//! Everything here decides how much a report *cannot* say. Sizes are never
//! reported exactly -- a file's size falls into one of a few
//! [`FileSizeBucket`]s, and the library's total into one of a few
//! [`TotalSizeBucket`]s -- and every free-form label (a container or codec
//! name) passes through [`normalize_label`] before it can leave the process.
//!
//! Pure functions and constants only: the SQL repository generates its
//! histogram from [`FileSizeBucket::ALL`], and the in-memory double buckets
//! with [`FileSizeBucket::of`], so the two cannot disagree on a boundary.

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

    /// Wire names are what a collector groups by, so two buckets sharing one
    /// would silently merge their counts.
    #[test]
    fn bucket_names_are_distinct_snake_case() {
        let names: Vec<&str> = FileSizeBucket::ALL
            .iter()
            .map(|b| b.as_str())
            .chain(TotalSizeBucket::ALL.iter().map(|b| b.as_str()))
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
