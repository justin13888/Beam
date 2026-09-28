//! Which of a title's files plays by default (issue #189).
//!
//! A movie or an episode may have several files -- another resolution,
//! another encode, another edition -- and one of them is the one a client
//! plays when the viewer has not chosen. It used to be whichever the database
//! returned first. It is now decided here, at read time, from what the files
//! are: nothing is stored, so a file added, removed or re-probed moves the
//! choice with it and no row can go stale.

use std::cmp::Ordering;

use uuid::Uuid;

/// What a source is ranked by, most significant first. A multi-part movie's
/// parts are one source ([`stack_parts`]), ranked by its first part's picture
/// and its parts' total size: its parts are one encode, so it competes with
/// another copy of its edition as a whole file would (decision D233-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRankKey {
    /// The source belongs to the title's default edition -- one its filename
    /// names no edition for. An episode's sources always do.
    pub is_default_edition: bool,
    /// The height of its first video stream; zero with none.
    pub height: u32,
    /// The bit rate of its first video stream; zero when unknown.
    pub video_bit_rate: u64,
    pub size_bytes: u64,
    /// The tiebreak: two files never share an id, so the order is total. A
    /// stacked source's is its first part's.
    pub file_id: Uuid,
}

impl SourceRankKey {
    /// `self` against `other` in rank order: the better source is `Less`.
    ///
    /// The default edition first; then the taller picture, the higher video
    /// bit rate and the larger file, each the likelier to be the better
    /// encode; then the lower file id, which only makes the order total.
    pub fn rank_cmp(&self, other: &Self) -> Ordering {
        let Self {
            is_default_edition,
            height,
            video_bit_rate,
            size_bytes,
            file_id,
        } = self;
        other
            .is_default_edition
            .cmp(is_default_edition)
            .then_with(|| other.height.cmp(height))
            .then_with(|| other.video_bit_rate.cmp(video_bit_rate))
            .then_with(|| other.size_bytes.cmp(size_bytes))
            .then_with(|| file_id.cmp(&other.file_id))
    }
}

/// Sort `sources` into rank order, the primary first.
pub fn rank_sources<T>(sources: &mut [T], key: impl Fn(&T) -> SourceRankKey) {
    sources.sort_by(|a, b| key(a).rank_cmp(&key(b)));
}

/// Which files play as one source (issue #233): the parts of one multi-part
/// movie, in part order. Every other file is a source of its own.
///
/// `part_of` names a file's stack and its part number, or `None` for a whole
/// file. The parts of one stack are one source when their numbers are
/// distinct; a stack in which two files claim one number cannot say which
/// plays, so each of its files is a source of its own instead. Sources come
/// out whole files first, in the order given, then stacks in stack order;
/// [`rank_sources`] decides the order that matters.
pub fn stack_parts<T, S: Ord>(
    files: Vec<T>,
    part_of: impl Fn(&T) -> Option<(S, u32)>,
) -> Vec<Vec<T>> {
    let mut sources: Vec<Vec<T>> = Vec::new();
    let mut stacks: std::collections::BTreeMap<S, Vec<(u32, T)>> =
        std::collections::BTreeMap::new();
    for file in files {
        match part_of(&file) {
            Some((stack, part)) => stacks.entry(stack).or_default().push((part, file)),
            None => sources.push(vec![file]),
        }
    }
    for (_, mut parts) in stacks {
        parts.sort_by_key(|(part, _)| *part);
        let distinct = parts.windows(2).all(|pair| pair[0].0 != pair[1].0);
        if distinct {
            sources.push(parts.into_iter().map(|(_, file)| file).collect());
        } else {
            sources.extend(parts.into_iter().map(|(_, file)| vec![file]));
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(
        is_default_edition: bool,
        height: u32,
        bit_rate: u64,
        size: u64,
        id: u128,
    ) -> SourceRankKey {
        SourceRankKey {
            is_default_edition,
            height,
            video_bit_rate: bit_rate,
            size_bytes: size,
            file_id: Uuid::from_u128(id),
        }
    }

    /// Each rule, with every less significant one pointing the other way so
    /// that only the rule under test can decide.
    #[test]
    fn each_rule_outranks_every_rule_after_it() {
        let cases = [
            (
                "the default edition beats a taller named edition",
                key(true, 720, 1, 1, 9),
                key(false, 2160, 9, 9, 1),
            ),
            (
                "the taller picture beats a higher bit rate",
                key(true, 2160, 1, 1, 9),
                key(true, 1080, 9, 9, 1),
            ),
            (
                "the higher bit rate beats a larger file",
                key(true, 1080, 9, 1, 9),
                key(true, 1080, 1, 9, 1),
            ),
            (
                "the larger file beats a lower id",
                key(true, 1080, 5, 9, 9),
                key(true, 1080, 5, 1, 1),
            ),
            (
                "the lower id breaks a tie",
                key(true, 1080, 5, 5, 1),
                key(true, 1080, 5, 5, 2),
            ),
        ];
        for (name, better, worse) in cases {
            assert_eq!(better.rank_cmp(&worse), Ordering::Less, "{name}");
            assert_eq!(worse.rank_cmp(&better), Ordering::Greater, "{name}");
        }
    }

    /// A file for [`stack_parts`]: its name, and its stack and part.
    type Part = (&'static str, Option<(&'static str, u32)>);

    fn stacked(files: Vec<Part>) -> Vec<Vec<&'static str>> {
        stack_parts(files, |(_, part)| *part)
            .into_iter()
            .map(|source| source.into_iter().map(|(name, _)| name).collect())
            .collect()
    }

    #[test]
    fn the_parts_of_one_stack_are_one_source_in_part_order() {
        assert_eq!(
            stacked(vec![
                ("cd2", Some(("a", 2))),
                ("whole", None),
                ("cd1", Some(("a", 1))),
                ("other cd1", Some(("b", 1))),
            ]),
            vec![vec!["whole"], vec!["cd1", "cd2"], vec!["other cd1"]]
        );
    }

    /// Two files claiming one part leave no way to say which plays: every
    /// file of that stack is a source of its own, and other stacks are
    /// untouched.
    #[test]
    fn a_stack_with_a_repeated_part_is_not_stacked() {
        assert_eq!(
            stacked(vec![
                ("a cd1", Some(("a", 1))),
                ("a cd1 again", Some(("a", 1))),
                ("a cd2", Some(("a", 2))),
                ("b cd1", Some(("b", 1))),
                ("b cd2", Some(("b", 2))),
            ]),
            vec![
                vec!["a cd1"],
                vec!["a cd1 again"],
                vec!["a cd2"],
                vec!["b cd1", "b cd2"]
            ]
        );
    }

    fn any_key() -> impl Strategy<Value = SourceRankKey> {
        (any::<bool>(), 0_u32..4, 0_u64..4, 0_u64..4, any::<u128>()).prop_map(
            |(default, height, rate, size, id)| key(default, height * 720, rate, size, id),
        )
    }

    proptest! {
        /// The ranking depends on the sources, never on the order they were
        /// read in, and every source keeps its place in it.
        #[test]
        fn ranking_is_the_same_whatever_order_the_sources_arrive_in(
            keys in prop::collection::vec(any_key(), 0..8),
            rotation in any::<usize>(),
        ) {
            let mut ranked = keys.clone();
            rank_sources(&mut ranked, |k| *k);

            let mut shuffled = keys.clone();
            shuffled.reverse();
            if !shuffled.is_empty() {
                let by = rotation % shuffled.len();
                shuffled.rotate_left(by);
            }
            rank_sources(&mut shuffled, |k| *k);
            prop_assert_eq!(&ranked, &shuffled);

            prop_assert_eq!(ranked.len(), keys.len());
            for key in &keys {
                prop_assert!(ranked.contains(key));
            }
            for pair in ranked.windows(2) {
                prop_assert_ne!(pair[0].rank_cmp(&pair[1]), Ordering::Greater);
            }
        }

        /// Stacking loses and duplicates no file, never mixes two stacks or
        /// a whole file into one source, and plays every source in part
        /// order.
        #[test]
        fn stacking_partitions_the_files(
            files in prop::collection::vec(prop::option::of((0_u8..3, 1_u32..4)), 0..10),
        ) {
            let numbered: Vec<(usize, Option<(u8, u32)>)> =
                files.iter().copied().enumerate().collect();
            let sources = stack_parts(numbered.clone(), |(_, part)| *part);

            let mut seen: Vec<usize> = sources.iter().flatten().map(|(i, _)| *i).collect();
            seen.sort_unstable();
            prop_assert_eq!(seen, (0..files.len()).collect::<Vec<_>>());
            for source in &sources {
                prop_assert!(!source.is_empty());
                if source.len() > 1 {
                    let stacks: std::collections::BTreeSet<u8> =
                        source.iter().filter_map(|(_, part)| part.map(|(s, _)| s)).collect();
                    prop_assert_eq!(stacks.len(), 1, "{:?}", source);
                    prop_assert!(source.iter().all(|(_, part)| part.is_some()), "{:?}", source);
                    for pair in source.windows(2) {
                        prop_assert!(pair[0].1.unwrap().1 < pair[1].1.unwrap().1, "{:?}", source);
                    }
                }
            }
        }
    }
}
