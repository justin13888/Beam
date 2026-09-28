//! A user's watched state for one title (issue #188).
//!
//! State is kept per *title* -- a movie, or one episode of a show -- not per
//! file: a movie's editions and an episode's encodes are sources of one title,
//! so switching from the 1080p file to the 4K one resumes where the viewer
//! stopped. The file last played is remembered beside the position, so a
//! client resumes on the source the viewer chose when it is still there.
//!
//! "Played" is sticky. Crossing [`COMPLETED_THRESHOLD`] of the duration marks
//! the title played, counts a play and resets the position to the start;
//! rewinding afterwards is a rewatch, never an unplay. Only an explicit
//! "mark unwatched" clears it.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The fraction of the duration past which a report counts as having
/// watched the title to the end.
pub const COMPLETED_THRESHOLD: f64 = 0.95;

/// The title one row of watched state belongs to.
///
/// An episode carries its show so a show's rows are read, dismissed and
/// collapsed without a join; the store holds the pair consistent with a
/// foreign key to each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchTarget {
    Movie { movie_id: Uuid },
    Episode { episode_id: Uuid, show_id: Uuid },
}

impl WatchTarget {
    /// The title a continue-watching row collapses this target into: the
    /// movie itself, or the episode's show.
    #[must_use]
    pub fn title(self) -> TitleRef {
        match self {
            Self::Movie { movie_id } => TitleRef::Movie(movie_id),
            Self::Episode { show_id, .. } => TitleRef::Show(show_id),
        }
    }
}

/// A top-level title: what continue-watching lists one row per.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitleRef {
    Movie(Uuid),
    Show(Uuid),
}

impl TitleRef {
    /// The movie's or the show's id.
    #[must_use]
    pub fn id(self) -> Uuid {
        match self {
            Self::Movie(id) | Self::Show(id) => id,
        }
    }
}

/// One user's state for one movie or episode.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchState {
    pub id: Uuid,
    pub user_id: Uuid,
    pub target: WatchTarget,
    /// The file the last report named; `None` once that file is purged.
    pub last_file_id: Option<Uuid>,
    /// Where to resume, in seconds. Zero for a title never started, and for
    /// one just finished.
    pub position_secs: f64,
    /// The duration the position was measured against.
    pub duration_secs: Option<f64>,
    /// Watched at least once. Never cleared by a report.
    pub completed: bool,
    /// How many times the title was watched to the end, or marked watched.
    pub play_count: u32,
    pub last_played_at: DateTime<Utc>,
    /// When the user last removed the title from continue-watching.
    pub dismissed_at: Option<DateTime<Utc>>,
}

impl WatchState {
    /// Whether a client should offer to resume: there is a position to
    /// resume from. A finished title rewound and partly rewatched resumes.
    #[must_use]
    pub fn is_resumable(&self) -> bool {
        self.position_secs > 0.0
    }

    /// Whether the title sits at its end: played, and not being rewatched.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.completed && !self.is_resumable()
    }
}

/// A validated progress report, resolved to its title.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordProgress {
    pub user_id: Uuid,
    pub target: WatchTarget,
    pub file_id: Uuid,
    pub position_secs: f64,
    /// The duration `position_secs` is measured against, if known: the
    /// reported file's own.
    pub duration_secs: Option<f64>,
    /// Whether the end of the reported file is the end of the title. It is
    /// not for a part of a multi-part movie before its last: that part's end
    /// is only where the next one starts.
    pub finishes_title: bool,
}

impl RecordProgress {
    /// Whether this report reaches the end of the title: at or past
    /// [`COMPLETED_THRESHOLD`] of a known, positive duration, in a file that
    /// finishes the title. A report with no duration never completes -- a
    /// still-probing or corrupt file reports none, and would otherwise mark
    /// any position finished.
    #[must_use]
    pub fn reaches_end(&self) -> bool {
        self.finishes_title
            && self
                .duration_secs
                .is_some_and(|d| d > 0.0 && self.position_secs >= d * COMPLETED_THRESHOLD)
    }
}

/// What a row holds after a report or a mark, given what it held before.
///
/// The one statement of the sticky-played rules, which the SQL upsert
/// restates as `CASE` expressions and the shared contract holds both to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Played {
    pub position_secs: f64,
    pub completed: bool,
    pub play_count: u32,
}

impl Played {
    /// A row that has never been written.
    pub const NEVER: Self = Self {
        position_secs: 0.0,
        completed: false,
        play_count: 0,
    };

    /// The row after watching to the end: played, back at the start, and one
    /// more play -- unless it already sat at its end, so a player that keeps
    /// reporting past 95% (or a repeated "mark watched") counts one play,
    /// not one per report.
    #[must_use]
    pub fn finished(self) -> Self {
        let already_at_end = self.completed && self.position_secs <= 0.0;
        Self {
            position_secs: 0.0,
            completed: true,
            play_count: if already_at_end {
                self.play_count
            } else {
                self.play_count.saturating_add(1)
            },
        }
    }

    /// The row after a report short of the end: at `position_secs`, and
    /// played exactly as before -- a rewind never unplays.
    #[must_use]
    pub fn at(self, position_secs: f64) -> Self {
        Self {
            position_secs,
            ..self
        }
    }
}

/// Where a history page ends: its last row's `(last_played_at, id)`, the
/// order history is read in, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryPosition {
    pub last_played_at: DateTime<Utc>,
    pub id: Uuid,
}

/// A title continue-watching considers, with the newest `last_played_at`
/// among its rows. Candidates are read newest `(last_played_at, title id)`
/// first, and a page of them resumes after the last one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueCandidate {
    pub title: TitleRef,
    pub last_played_at: DateTime<Utc>,
}

impl From<&WatchState> for HistoryPosition {
    fn from(state: &WatchState) -> Self {
        Self {
            last_played_at: state.last_played_at,
            id: state.id,
        }
    }
}

#[cfg(feature = "entity")]
impl TryFrom<beam_entity::watch_state::Model> for WatchState {
    type Error = sea_orm::DbErr;

    fn try_from(model: beam_entity::watch_state::Model) -> Result<Self, Self::Error> {
        let beam_entity::watch_state::Model {
            id,
            user_id,
            movie_id,
            episode_id,
            show_id,
            last_file_id,
            position_secs,
            duration_secs,
            completed,
            play_count,
            last_played_at,
            dismissed_at,
        } = model;
        // The table's CHECKs hold exactly one of the two shapes; a row with
        // neither is refused here rather than guessed at.
        let target = match (movie_id, episode_id, show_id) {
            (Some(movie_id), None, None) => WatchTarget::Movie { movie_id },
            (None, Some(episode_id), Some(show_id)) => WatchTarget::Episode {
                episode_id,
                show_id,
            },
            _ => {
                return Err(sea_orm::DbErr::Custom(format!(
                    "watch_state {id} names neither one movie nor one episode of a show"
                )));
            }
        };
        Ok(Self {
            id,
            user_id,
            target,
            last_file_id,
            position_secs,
            duration_secs,
            completed,
            play_count: u32::try_from(play_count).unwrap_or(0),
            last_played_at: last_played_at.with_timezone(&Utc),
            dismissed_at: dismissed_at.map(|at| at.with_timezone(&Utc)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn report(position_secs: f64, duration_secs: Option<f64>) -> RecordProgress {
        RecordProgress {
            user_id: Uuid::nil(),
            target: WatchTarget::Movie {
                movie_id: Uuid::nil(),
            },
            file_id: Uuid::nil(),
            position_secs,
            duration_secs,
            finishes_title: true,
        }
    }

    #[test]
    fn the_end_is_the_threshold_of_a_known_positive_duration() {
        for (position, duration, ends) in [
            (94.9, Some(100.0), false),
            (95.0, Some(100.0), true),
            (100.0, Some(100.0), true),
            (1_000_000.0, None, false),
            // A zero duration is what a still-probing or corrupt file
            // reports: no position on it is the end.
            (0.0, Some(0.0), false),
            (1_000.0, Some(0.0), false),
        ] {
            assert_eq!(
                report(position, duration).reaches_end(),
                ends,
                "{position} of {duration:?}"
            );
        }
    }

    #[test]
    fn a_file_that_does_not_finish_the_title_never_reaches_its_end() {
        for position in [95.0, 100.0] {
            let report = RecordProgress {
                finishes_title: false,
                ..report(position, Some(100.0))
            };
            assert!(!report.reaches_end(), "{position} of an earlier part");
        }
    }

    #[test]
    fn finishing_counts_one_play_per_viewing_not_per_report() {
        let first = Played::NEVER.finished();
        assert_eq!(
            first,
            Played {
                position_secs: 0.0,
                completed: true,
                play_count: 1
            }
        );
        assert_eq!(first.finished(), first, "a second report at the end");
        let rewatched = first.at(30.0).finished();
        assert_eq!(rewatched.play_count, 2, "a rewatch to the end is a play");
    }

    #[test]
    fn a_report_short_of_the_end_moves_the_position_and_keeps_played() {
        let played = Played::NEVER.finished();
        assert_eq!(
            played.at(5.0),
            Played {
                position_secs: 5.0,
                completed: true,
                play_count: 1
            },
            "a rewind is a rewatch, never an unplay"
        );
        assert!(!Played::NEVER.at(5.0).completed);
    }

    proptest! {
        /// Whatever sequence of reports and marks a row sees, `completed`
        /// never goes back to false and the play count never falls.
        #[test]
        fn played_is_monotonic(steps in prop::collection::vec(prop::option::of(0.0f64..10_000.0), 0..40)) {
            let mut row = Played::NEVER;
            for step in steps {
                let next = match step {
                    Some(position) => row.at(position),
                    None => row.finished(),
                };
                prop_assert!(!row.completed || next.completed);
                prop_assert!(next.play_count >= row.play_count);
                prop_assert!(next.play_count <= row.play_count + 1);
                prop_assert_eq!(next.completed, next.play_count > 0);
                row = next;
            }
        }
    }
}
