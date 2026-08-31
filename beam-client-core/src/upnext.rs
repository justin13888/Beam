//! Which episode plays when the current one ends.
//!
//! A direct port of `beam-web/src/lib/upNext.ts`, semantics included. Those
//! semantics are already ratified and covered by the web client's own suite,
//! so this is a translation rather than a redesign, and the tests below mirror
//! that file's cases one-for-one. Divergence between the two clients on
//! something as visible as auto-advance would be a bug in whichever moved.

/// One episode, reduced to what auto-advance actually needs.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpNextEpisode {
    /// The episode's identifier.
    pub id: String,
    /// Its number within the season.
    pub episode_number: i32,
    /// The file backing it, when one is indexed.
    pub file_id: Option<String>,
}

/// One season, reduced likewise.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpNextSeason {
    /// The season's number.
    pub season_number: i32,
    /// Its episodes, in whatever order the payload supplied.
    pub episodes: Vec<UpNextEpisode>,
}

impl UpNextEpisode {
    /// Whether this episode has a file that can actually be played.
    ///
    /// An empty `file_id` counts as absent. The web client tests this case
    /// explicitly, and it is the sort of thing that silently produces a
    /// request for `/v1/files//stream` if left unchecked.
    fn is_playable(&self) -> bool {
        self.file_id.as_deref().is_some_and(|id| !id.is_empty())
    }
}

/// The next playable episode after `current_episode_id`.
///
/// Crosses season boundaries and skips episodes with no indexed file.
/// Returns `None` when the current episode is the last playable one, when the
/// id is not present, or when there is nothing to advance through.
#[must_use]
pub fn next_playable_episode(
    seasons: &[UpNextSeason],
    current_episode_id: &str,
) -> Option<UpNextEpisode> {
    if current_episode_id.is_empty() {
        return None;
    }

    // Sorted defensively rather than trusting the payload's order: the helper
    // is cheap and pure, and a mis-ordered response must not cause playback
    // to skip an episode.
    let mut ordered: Vec<(i32, i32, &UpNextEpisode)> = seasons
        .iter()
        .flat_map(|season| {
            season
                .episodes
                .iter()
                .map(move |episode| (season.season_number, episode.episode_number, episode))
        })
        .collect();
    ordered.sort_by_key(|(season, episode, _)| (*season, *episode));

    let current = ordered
        .iter()
        .position(|(_, _, episode)| episode.id == current_episode_id)?;

    ordered
        .iter()
        .skip(current + 1)
        .find(|(_, _, episode)| episode.is_playable())
        .map(|(_, _, episode)| (*episode).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: &str, number: i32, file_id: Option<&str>) -> UpNextEpisode {
        UpNextEpisode {
            id: id.to_owned(),
            episode_number: number,
            file_id: file_id.map(str::to_owned),
        }
    }

    fn seasons() -> Vec<UpNextSeason> {
        vec![
            UpNextSeason {
                season_number: 1,
                episodes: vec![
                    episode("s1e1", 1, Some("f1")),
                    episode("s1e2", 2, Some("f2")),
                    episode("s1e3", 3, None),
                ],
            },
            UpNextSeason {
                season_number: 2,
                episodes: vec![episode("s2e1", 1, Some("f4")), episode("s2e2", 2, None)],
            },
        ]
    }

    #[test]
    fn returns_the_next_episode_in_the_same_season() {
        let next = next_playable_episode(&seasons(), "s1e1").expect("s1e2");
        assert_eq!(next.id, "s1e2");
    }

    #[test]
    fn skips_file_less_episodes_crossing_the_season_boundary() {
        // s1e3 has no file, so advancing from s1e2 must reach s2e1.
        let next = next_playable_episode(&seasons(), "s1e2").expect("s2e1");
        assert_eq!(next.id, "s2e1");
    }

    #[test]
    fn advances_from_a_file_less_current_episode_too() {
        let next = next_playable_episode(&seasons(), "s1e3").expect("s2e1");
        assert_eq!(next.id, "s2e1");
    }

    #[test]
    fn returns_none_after_the_last_playable_episode() {
        assert_eq!(next_playable_episode(&seasons(), "s2e1"), None);
    }

    #[test]
    fn returns_none_when_only_file_less_episodes_remain() {
        assert_eq!(next_playable_episode(&seasons(), "s2e2"), None);
    }

    #[test]
    fn returns_none_when_the_current_episode_id_is_not_found() {
        assert_eq!(next_playable_episode(&seasons(), "nope"), None);
    }

    #[test]
    fn returns_none_for_an_empty_current_episode_id() {
        // The Rust equivalent of the web client's null/undefined case.
        assert_eq!(next_playable_episode(&seasons(), ""), None);
    }

    #[test]
    fn returns_none_when_there_are_no_seasons() {
        assert_eq!(next_playable_episode(&[], "s1e1"), None);
    }

    #[test]
    fn orders_by_season_and_episode_number_not_input_order() {
        let shuffled = vec![
            UpNextSeason {
                season_number: 2,
                episodes: vec![episode("s2e1", 1, Some("f4"))],
            },
            UpNextSeason {
                season_number: 1,
                episodes: vec![
                    episode("s1e2", 2, Some("f2")),
                    episode("s1e1", 1, Some("f1")),
                ],
            },
        ];
        let next = next_playable_episode(&shuffled, "s1e1").expect("s1e2");
        assert_eq!(next.id, "s1e2");
    }

    #[test]
    fn does_not_treat_an_empty_string_file_id_as_playable() {
        let seasons = vec![UpNextSeason {
            season_number: 1,
            episodes: vec![
                episode("s1e1", 1, Some("f1")),
                episode("s1e2", 2, Some("")),
                episode("s1e3", 3, Some("f3")),
            ],
        }];
        let next = next_playable_episode(&seasons, "s1e1").expect("s1e3");
        assert_eq!(next.id, "s1e3", "an empty file id must be skipped");
    }
}
