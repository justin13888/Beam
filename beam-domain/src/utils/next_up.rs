//! Which episode of a show a viewer watches next (issue #188).
//!
//! The server decides, so every client shows the same row: continue-watching
//! collapses a show into one entry, and this is the entry.
//!
//! The *anchor* is the episode the viewer touched last -- the latest
//! `last_played_at`, ties broken towards the later episode, so the episodes
//! a multi-episode file marks at once anchor on the last of them. Then:
//!
//! * the anchor has a position and a file to play it from: resume it;
//! * otherwise the first episode after it, in `(season, episode)` order, that
//!   has a file and is not already played: start that one;
//! * no such episode: the viewer is caught up, and the show is not listed.
//!
//! Seasons are crossed, so the end of season 1 leads to the start of season
//! 2. Season 0 -- specials -- sorts first, as its number says: a viewer who
//! finished a special is led into season 1, and one working through season 1
//! is never pulled back into the specials.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One episode of a show, as next-up reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlineEpisode {
    pub episode_id: Uuid,
    pub season_number: u32,
    pub episode_number: u32,
    /// The episode has at least one present file.
    pub playable: bool,
}

/// What the viewer has done with one episode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpisodeWatch {
    pub episode_id: Uuid,
    pub position_secs: f64,
    pub completed: bool,
    pub last_played_at: DateTime<Utc>,
}

/// The episode a show's continue-watching row offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextUp {
    /// Carry on with an episode already started.
    Resume { episode_id: Uuid },
    /// Start the episode after the last one watched.
    Next { episode_id: Uuid },
    /// Nothing is left to watch after where the viewer is.
    Finished,
}

/// The episode to offer, from the show's episodes and the viewer's state
/// for some of them. States for episodes not in `outline` are ignored.
#[must_use]
pub fn next_up(outline: &[OutlineEpisode], watched: &[EpisodeWatch]) -> NextUp {
    let mut ordered: Vec<&OutlineEpisode> = outline.iter().collect();
    ordered.sort_by_key(|e| (e.season_number, e.episode_number, e.episode_id));
    let position_of = |episode_id: Uuid| ordered.iter().position(|e| e.episode_id == episode_id);

    let Some((anchor_index, anchor)) = watched
        .iter()
        .filter_map(|watch| position_of(watch.episode_id).map(|index| (index, watch)))
        .max_by_key(|(index, watch)| (watch.last_played_at, *index))
    else {
        return NextUp::Finished;
    };

    if anchor.position_secs > 0.0 && ordered[anchor_index].playable {
        return NextUp::Resume {
            episode_id: anchor.episode_id,
        };
    }

    let completed = |episode_id: Uuid| {
        watched
            .iter()
            .any(|watch| watch.episode_id == episode_id && watch.completed)
    };
    ordered[anchor_index + 1..]
        .iter()
        .find(|e| e.playable && !completed(e.episode_id))
        .map_or(NextUp::Finished, |e| NextUp::Next {
            episode_id: e.episode_id,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + minutes * 60, 0).unwrap()
    }

    fn id(season: u32, episode: u32) -> Uuid {
        Uuid::from_u128(u128::from(season) * 1_000 + u128::from(episode))
    }

    fn episode(season: u32, episode: u32) -> OutlineEpisode {
        OutlineEpisode {
            episode_id: id(season, episode),
            season_number: season,
            episode_number: episode,
            playable: true,
        }
    }

    fn unplayable(season: u32, number: u32) -> OutlineEpisode {
        OutlineEpisode {
            playable: false,
            ..episode(season, number)
        }
    }

    fn finished(season: u32, episode: u32, minute: i64) -> EpisodeWatch {
        EpisodeWatch {
            episode_id: id(season, episode),
            position_secs: 0.0,
            completed: true,
            last_played_at: at(minute),
        }
    }

    fn started(season: u32, episode: u32, position_secs: f64, minute: i64) -> EpisodeWatch {
        EpisodeWatch {
            episode_id: id(season, episode),
            position_secs,
            completed: false,
            last_played_at: at(minute),
        }
    }

    fn next(season: u32, episode: u32) -> NextUp {
        NextUp::Next {
            episode_id: id(season, episode),
        }
    }

    fn resume(season: u32, episode: u32) -> NextUp {
        NextUp::Resume {
            episode_id: id(season, episode),
        }
    }

    #[test]
    fn the_table() {
        let s1 = [episode(1, 1), episode(1, 2), episode(1, 3)];
        let two_seasons = [episode(1, 1), episode(1, 2), episode(2, 1), episode(2, 2)];
        let with_specials = [episode(0, 1), episode(1, 1), episode(1, 2)];
        let gap = [episode(1, 1), unplayable(1, 2), episode(1, 3)];
        // Listed out of order: the outline's order is the numbers', not the
        // store's.
        let shuffled = [episode(2, 1), episode(1, 2), episode(1, 1)];

        let cases: Vec<(&str, &[OutlineEpisode], Vec<EpisodeWatch>, NextUp)> = vec![
            ("nothing watched", &s1, vec![], NextUp::Finished),
            ("finished E1", &s1, vec![finished(1, 1, 0)], next(1, 2)),
            (
                "half way through E2",
                &s1,
                vec![finished(1, 1, 0), started(1, 2, 300.0, 1)],
                resume(1, 2),
            ),
            (
                "finished the last",
                &s1,
                vec![finished(1, 3, 0)],
                NextUp::Finished,
            ),
            (
                "the end of a season leads into the next",
                &two_seasons,
                vec![finished(1, 2, 0)],
                next(2, 1),
            ),
            (
                "the numbers order, not the listing",
                &shuffled,
                vec![finished(1, 2, 0)],
                next(2, 1),
            ),
            (
                "a special leads into season 1",
                &with_specials,
                vec![finished(0, 1, 0)],
                next(1, 1),
            ),
            (
                "season 1 never leads back into the specials",
                &with_specials,
                vec![finished(1, 1, 0)],
                next(1, 2),
            ),
            (
                "an episode with no file is stepped over",
                &gap,
                vec![finished(1, 1, 0)],
                next(1, 3),
            ),
            (
                "an episode already played is stepped over",
                &s1,
                vec![finished(1, 2, 0), finished(1, 1, 1)],
                next(1, 3),
            ),
            (
                "the latest touched anchors, not the furthest",
                &s1,
                vec![finished(1, 3, 0), finished(1, 1, 1)],
                next(1, 2),
            ),
            (
                "a tie anchors on the later episode",
                &s1,
                vec![finished(1, 1, 0), finished(1, 2, 0)],
                next(1, 3),
            ),
            (
                "a rewatch of a played episode resumes",
                &s1,
                vec![EpisodeWatch {
                    completed: true,
                    ..started(1, 2, 60.0, 0)
                }],
                resume(1, 2),
            ),
            (
                "a started episode whose file is gone moves on",
                &gap,
                vec![started(1, 2, 60.0, 0)],
                next(1, 3),
            ),
            (
                "state for an episode no longer in the show is ignored",
                &s1,
                vec![finished(1, 1, 0), finished(9, 9, 5)],
                next(1, 2),
            ),
        ];
        for (why, outline, watched, expected) in cases {
            assert_eq!(next_up(outline, &watched), expected, "{why}");
        }
    }

    fn outline_strategy() -> impl Strategy<Value = Vec<OutlineEpisode>> {
        prop::collection::btree_set((0u32..4, 1u32..8), 0..20).prop_flat_map(|numbers| {
            let numbers: Vec<(u32, u32)> = numbers.into_iter().collect();
            let len = numbers.len();
            prop::collection::vec(any::<bool>(), len).prop_map(move |playable| {
                numbers
                    .iter()
                    .zip(playable)
                    .map(|(&(season, number), playable)| OutlineEpisode {
                        playable,
                        ..episode(season, number)
                    })
                    .collect()
            })
        })
    }

    proptest! {
        /// Whatever the show and the viewer's state, the offer is a playable
        /// episode of the show; a fresh start is never an episode already
        /// played; and "finished" means nothing playable and unplayed follows
        /// the anchor.
        #[test]
        fn the_offer_is_always_one_the_viewer_can_play(
            outline in outline_strategy(),
            picks in prop::collection::vec((0usize..20, 0.0f64..100.0, any::<bool>(), 0i64..5), 0..10),
        ) {
            let mut watched: Vec<EpisodeWatch> = Vec::new();
            for (pick, position, completed, minute) in picks {
                if let Some(episode) = outline.get(pick % outline.len().max(1))
                    && watched.iter().all(|w| w.episode_id != episode.episode_id)
                {
                    watched.push(EpisodeWatch {
                        episode_id: episode.episode_id,
                        position_secs: position,
                        completed,
                        last_played_at: at(minute),
                    });
                }
            }
            let find = |id: Uuid| outline.iter().find(|e| e.episode_id == id).copied();
            match next_up(&outline, &watched) {
                NextUp::Resume { episode_id } => {
                    let episode = find(episode_id).expect("an episode of the show");
                    prop_assert!(episode.playable);
                    let watch = watched.iter().find(|w| w.episode_id == episode_id).expect("started");
                    prop_assert!(watch.position_secs > 0.0);
                }
                NextUp::Next { episode_id } => {
                    let episode = find(episode_id).expect("an episode of the show");
                    prop_assert!(episode.playable);
                    prop_assert!(!watched.iter().any(|w| w.episode_id == episode_id && w.completed));
                }
                NextUp::Finished => {
                    if let Some(anchor) = watched.iter().max_by_key(|w| {
                        let e = find(w.episode_id).unwrap();
                        (w.last_played_at, e.season_number, e.episode_number)
                    }) {
                        let a = find(anchor.episode_id).unwrap();
                        let played = |id: Uuid| watched.iter().any(|w| w.episode_id == id && w.completed);
                        let unplayed_after = outline.iter().any(|e| {
                            (e.season_number, e.episode_number) > (a.season_number, a.episode_number)
                                && e.playable
                                && !played(e.episode_id)
                        });
                        prop_assert!(!unplayed_after, "finished with an episode left to play");
                    }
                }
            }
        }
    }
}
