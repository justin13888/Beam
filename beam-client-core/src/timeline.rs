//! Where a source's parts sit end to end, so a position in one source can be
//! carried to another.
//!
//! A multi-part movie (issue #233) is queued one item per part, and the player
//! reports positions within the part it is in. Switching to another version of
//! the same title has to land at the same moment of the film, which means
//! turning "part 2, 10 minutes in" into "1 hour 40 minutes in" and back. A part
//! whose duration is unknown ends the timeline there: nothing after it can be
//! placed, so a position past it stays in it rather than being guessed into a
//! later part.
//!
//! Positions are milliseconds as a platform player reports them, signed
//! because that is the player's own type (Media3's `Long`); a negative one is
//! read as the start.

use crate::capability::select::PlaybackPart;

/// A position within one part of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PartPosition {
    /// Which part, from 0.
    pub index: u32,
    /// How far into it, in milliseconds.
    pub offset_ms: i64,
}

/// Each part's duration in milliseconds, in play order; `None` where unknown.
fn durations_ms(parts: &[PlaybackPart]) -> impl Iterator<Item = Option<i64>> + '_ {
    parts.iter().map(|part| {
        // Truncated, as the player's own millisecond clock is.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a duration in milliseconds is far inside i64; `as` saturates beyond it"
        )]
        part.duration_secs.map(|secs| (secs * 1_000.0) as i64)
    })
}

/// How far into the whole source `offset_ms` into the part at `index` is.
///
/// From a part whose start cannot be known -- one after a part of unknown
/// length -- the offset is all there is.
#[uniffi::export]
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "an exported function receives its arguments owned from the foreign side"
)]
pub fn part_absolute_ms(parts: Vec<PlaybackPart>, index: u32, offset_ms: i64) -> i64 {
    let offset_ms = offset_ms.max(0);
    let mut before = 0_i64;
    for duration in durations_ms(&parts).take(index as usize) {
        let Some(duration) = duration else {
            return offset_ms;
        };
        before = before.saturating_add(duration);
    }
    before.saturating_add(offset_ms)
}

/// The part `absolute_ms` into the whole source falls in, and how far into it.
///
/// A moment past the end stays in the last part: the player clamps a seek
/// past the end itself, and a guessed part beyond the last would not exist.
#[uniffi::export]
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "an exported function receives its arguments owned from the foreign side"
)]
pub fn part_locate(parts: Vec<PlaybackPart>, absolute_ms: i64) -> PartPosition {
    let mut remaining = absolute_ms.max(0);
    let last = parts.len().saturating_sub(1);
    for (index, duration) in durations_ms(&parts).enumerate() {
        let position = PartPosition {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            offset_ms: remaining,
        };
        match duration {
            Some(duration) if index != last && remaining >= duration => remaining -= duration,
            _ => return position,
        }
    }
    PartPosition {
        index: 0,
        offset_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(durations_secs: &[Option<f64>]) -> Vec<PlaybackPart> {
        durations_secs
            .iter()
            .enumerate()
            .map(|(index, duration_secs)| PlaybackPart {
                file_id: format!("part-{index}"),
                url: format!("https://beam.example/{index}"),
                duration_secs: *duration_secs,
                sidecar_subtitles: Vec::new(),
            })
            .collect()
    }

    fn at(index: u32, offset_ms: i64) -> PartPosition {
        PartPosition { index, offset_ms }
    }

    fn two_parts() -> Vec<PlaybackPart> {
        parts(&[Some(3_000.0), Some(2_800.0)])
    }

    #[test]
    fn a_position_in_a_later_part_counts_every_part_before_it() {
        assert_eq!(part_absolute_ms(two_parts(), 1, 600_000), 3_600_000);
        assert_eq!(part_absolute_ms(two_parts(), 0, 600_000), 600_000);
    }

    #[test]
    fn a_moment_of_the_film_is_found_in_the_part_that_holds_it() {
        assert_eq!(part_locate(two_parts(), 2_999_999), at(0, 2_999_999));
        assert_eq!(part_locate(two_parts(), 3_000_000), at(1, 0));
        assert_eq!(part_locate(two_parts(), 3_600_000), at(1, 600_000));
    }

    #[test]
    fn locating_is_the_inverse_of_counting_for_every_part() {
        for index in 0..2 {
            for offset in [0, 1, 1_234_567] {
                let absolute = part_absolute_ms(two_parts(), index, offset);
                assert_eq!(part_locate(two_parts(), absolute), at(index, offset));
            }
        }
    }

    #[test]
    fn a_moment_past_the_end_stays_in_the_last_part() {
        assert_eq!(part_locate(two_parts(), 6_000_000), at(1, 3_000_000));
    }

    #[test]
    fn a_part_of_unknown_length_ends_what_can_be_placed() {
        let unknown = parts(&[Some(1.0), None, Some(1.0)]);
        assert_eq!(part_locate(unknown.clone(), 5_000), at(1, 4_000));
        // From a part whose start cannot be known, the offset is all there is.
        assert_eq!(part_absolute_ms(unknown, 2, 50), 50);
    }

    #[test]
    fn a_whole_file_maps_a_position_to_itself() {
        let whole = parts(&[None]);
        assert_eq!(part_locate(whole.clone(), 42_000), at(0, 42_000));
        assert_eq!(part_absolute_ms(whole, 0, 42_000), 42_000);
    }

    #[test]
    fn a_negative_position_reads_as_the_start() {
        assert_eq!(part_locate(two_parts(), -5), at(0, 0));
        assert_eq!(part_absolute_ms(two_parts(), 1, -5), 3_000_000);
    }

    #[test]
    fn an_index_past_the_last_part_counts_every_part() {
        assert_eq!(part_absolute_ms(two_parts(), 7, 10), 5_800_010);
    }

    #[test]
    fn a_source_with_no_parts_locates_to_the_start() {
        assert_eq!(part_locate(Vec::new(), 10_000), at(0, 0));
    }

    #[test]
    fn a_fractional_duration_is_truncated_to_the_millisecond() {
        let fractional = parts(&[Some(1.000_9), Some(1.0)]);
        assert_eq!(part_absolute_ms(fractional.clone(), 1, 0), 1_000);
        assert_eq!(part_locate(fractional, 1_000), at(1, 0));
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Every moment inside a known-length source is found in a part,
            /// and counting from that part gives the moment back.
            #[test]
            fn locate_then_count_is_the_identity_within_the_source(
                durations in proptest::collection::vec(1_u32..10_000_000, 1..6),
                fraction in 0.0_f64..1.0,
            ) {
                let source = parts(
                    &durations
                        .iter()
                        .map(|ms| Some(f64::from(*ms) / 1_000.0))
                        .collect::<Vec<_>>(),
                );
                let total: i64 = durations.iter().map(|ms| i64::from(*ms)).sum();
                #[expect(clippy::cast_possible_truncation, reason = "inside [0, total)")]
                let moment = (fraction * total as f64) as i64;

                let position = part_locate(source.clone(), moment);
                prop_assert!((position.index as usize) < durations.len());
                prop_assert_eq!(
                    part_absolute_ms(source, position.index, position.offset_ms),
                    moment
                );
            }
        }
    }
}
