//! The playback telemetry classes (issue #143): the height, bitrate and
//! rebuffer-duration ranges a playback counter is kept under. Each class is
//! read from one table of lower bounds, so a boundary lives in exactly one
//! place.

use crate::models::playback_telemetry::{BitrateClass, HeightClass, RebufferBucket};

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
