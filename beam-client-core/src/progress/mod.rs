//! Reporting where the viewer is, without reporting it constantly.
//!
//! The player emits a position several times a second. `beam-web` throttles
//! that to one request per fifteen seconds of playback per file
//! (`usePlaybackBeacon.ts`), with a forced path for pause, seek-end and
//! unmount where losing the last few seconds would be visible. This is the
//! same policy, in the core, so every native client inherits it rather than
//! each picking its own interval.

pub mod queue;

pub use queue::{ProgressQueue, QueuedProgress};

use crate::clock::Clock;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Minimum seconds of playback between two reports for the same file.
///
/// Matches `REPORT_INTERVAL_SECS` in `beam-web/src/hooks/usePlaybackBeacon.ts`.
/// The two clients drifting apart here would make resume behaviour differ by
/// platform for no reason a user could understand.
pub const REPORT_INTERVAL_SECS: i64 = 15;

/// What happened to a reported position.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ProgressOutcome {
    /// Sent to the server and acknowledged.
    Sent {
        /// The position the server now holds.
        position_secs: f64,
    },
    /// Held back by the throttle. The next eligible report carries its own,
    /// newer position rather than this one.
    Throttled {
        /// Seconds until a report for this file would be sent.
        next_eligible_in_secs: u64,
    },
    /// Could not be sent, and persisted for retry.
    Queued {
        /// How many samples are now waiting.
        pending: u32,
    },
    /// Could not be sent and will not be retried.
    Dropped {
        /// Why, phrased for a person.
        reason: String,
    },
}

/// What a held sample for one file still contributes to the next send,
/// superseded by any newer one.
///
/// Only the duration: the next send's own position is newer than any held
/// one, so a held position is never sent.
#[derive(Debug, Clone, Copy)]
struct Pending {
    duration_secs: Option<f64>,
}

/// Decides whether a position should be sent now, later, or coalesced.
///
/// Holds no transport of its own: it answers "should this go out?" and the
/// caller performs the send. That keeps the timing rule unit-testable without
/// a network, which is the whole reason it is a separate type.
#[derive(Debug)]
pub struct ProgressThrottle {
    clock: Arc<dyn Clock>,
    interval_secs: i64,
    last_sent: Mutex<HashMap<String, i64>>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// Whether a report should go out now.
#[derive(Debug, Clone, PartialEq)]
pub enum ThrottleDecision {
    /// Send this position.
    Send {
        /// The position to send: always the one just offered, which is the
        /// newest, even where a held sample was further along.
        position_secs: f64,
        /// The duration to send alongside it: the offered one, or a held
        /// sample's where the offered one is unknown.
        duration_secs: Option<f64>,
    },
    /// Hold back; the duration has been remembered for the next send.
    Hold {
        /// Seconds until this file becomes eligible.
        next_eligible_in_secs: u64,
    },
}

impl ProgressThrottle {
    /// A throttle using [`REPORT_INTERVAL_SECS`].
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_interval(clock, REPORT_INTERVAL_SECS)
    }

    /// A throttle with a custom interval.
    #[must_use]
    pub fn with_interval(clock: Arc<dyn Clock>, interval_secs: i64) -> Self {
        Self {
            clock,
            interval_secs,
            last_sent: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Decide what to do with a position.
    ///
    /// `force` bypasses the interval for point-in-time events -- pause, seek
    /// end, track change, player release -- where losing the last few seconds
    /// of progress is user-visible.
    pub fn decide(
        &self,
        file_id: &str,
        position_secs: f64,
        duration_secs: Option<f64>,
        force: bool,
    ) -> ThrottleDecision {
        let now = self.clock.now_unix();
        let last = self
            .last_sent
            .lock()
            .expect("last_sent lock")
            .get(file_id)
            .copied();

        let elapsed_enough = last.is_none_or(|last_sent| now - last_sent >= self.interval_secs);

        if force || elapsed_enough {
            // The sample being offered is the newest one, so its position wins
            // over anything held back -- even a smaller one. A smaller position
            // is a seek backward, which is the viewer's intent; preferring the
            // larger held one sent a stale resume point after every rewind.
            // The server's monotonic `Played` rule already protects watched
            // state, so nothing here needs to. A held sample still contributes
            // the duration where the newest one does not know it.
            let coalesced = self.pending.lock().expect("pending lock").remove(file_id);
            let duration_secs =
                duration_secs.or_else(|| coalesced.and_then(|held| held.duration_secs));
            self.last_sent
                .lock()
                .expect("last_sent lock")
                .insert(file_id.to_owned(), now);
            return ThrottleDecision::Send {
                position_secs,
                duration_secs,
            };
        }

        self.pending
            .lock()
            .expect("pending lock")
            .insert(file_id.to_owned(), Pending { duration_secs });
        let wait = last.map_or(0, |last_sent| self.interval_secs - (now - last_sent));
        ThrottleDecision::Hold {
            next_eligible_in_secs: u64::try_from(wait.max(0)).unwrap_or(0),
        }
    }

    /// Forget a file's throttle state, when playback of it ends.
    pub fn reset(&self, file_id: &str) {
        self.last_sent
            .lock()
            .expect("last_sent lock")
            .remove(file_id);
        self.pending.lock().expect("pending lock").remove(file_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;

    fn throttle(clock: &Arc<TestClock>) -> ProgressThrottle {
        ProgressThrottle::new(clock.clone())
    }

    #[test]
    fn the_first_report_for_a_file_is_always_sent() {
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        assert!(matches!(
            throttle.decide("f1", 10.0, Some(7200.0), false),
            ThrottleDecision::Send { .. }
        ));
    }

    #[test]
    fn a_second_report_inside_the_interval_is_held() {
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, None, false);

        clock.advance_secs(REPORT_INTERVAL_SECS - 1);
        assert_eq!(
            throttle.decide("f1", 24.0, None, false),
            ThrottleDecision::Hold {
                next_eligible_in_secs: 1
            }
        );
    }

    #[test]
    fn a_report_is_sent_once_the_interval_has_elapsed() {
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, None, false);

        clock.advance_secs(REPORT_INTERVAL_SECS);
        assert!(matches!(
            throttle.decide("f1", 25.0, None, false),
            ThrottleDecision::Send {
                position_secs: p, ..
            } if (p - 25.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn force_bypasses_the_interval() {
        // Pause, seek-end and player release all take this path: losing the
        // last few seconds there is immediately visible to the viewer.
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, None, false);

        clock.advance_secs(1);
        assert!(matches!(
            throttle.decide("f1", 11.0, None, true),
            ThrottleDecision::Send { .. }
        ));
    }

    #[test]
    fn a_forced_report_after_seeking_back_sends_the_new_position() {
        // The viewer watched to 20s, then rewound to 12s and paused. The
        // forced report must carry 12s: sending the larger held 20s would
        // put the resume point somewhere the viewer deliberately left.
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, Some(7200.0), false);

        clock.advance_secs(5);
        throttle.decide("f1", 15.0, Some(7200.0), false);
        clock.advance_secs(5);
        throttle.decide("f1", 20.0, Some(7200.0), false);

        clock.advance_secs(1);
        assert_eq!(
            throttle.decide("f1", 12.0, Some(7200.0), true),
            ThrottleDecision::Send {
                position_secs: 12.0,
                duration_secs: Some(7200.0),
            }
        );
    }

    #[test]
    fn a_held_duration_fills_a_send_that_does_not_know_one() {
        // A player that has lost the duration (a track change mid-report)
        // still reports the one an earlier, held sample carried.
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, Some(7200.0), false);
        clock.advance_secs(5);
        throttle.decide("f1", 15.0, Some(7200.0), false);

        clock.advance_secs(REPORT_INTERVAL_SECS);
        assert_eq!(
            throttle.decide("f1", 30.0, None, false),
            ThrottleDecision::Send {
                position_secs: 30.0,
                duration_secs: Some(7200.0),
            }
        );
    }

    #[test]
    fn a_send_s_own_duration_wins_over_a_held_one() {
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, Some(7200.0), false);
        clock.advance_secs(5);
        throttle.decide("f1", 15.0, Some(7200.0), false);

        assert_eq!(
            throttle.decide("f1", 16.0, Some(3600.0), true),
            ThrottleDecision::Send {
                position_secs: 16.0,
                duration_secs: Some(3600.0),
            }
        );
    }

    #[test]
    fn throttling_is_tracked_per_file_not_globally() {
        // Up-next advances to a new file mid-interval; its first report must
        // not be swallowed by the previous file's throttle.
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, None, false);

        assert!(matches!(
            throttle.decide("f2", 0.0, None, false),
            ThrottleDecision::Send { .. }
        ));
    }

    #[test]
    fn resetting_a_file_clears_both_its_timer_and_its_held_sample() {
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, None, false);
        clock.advance_secs(1);
        throttle.decide("f1", 11.0, Some(7200.0), false);

        throttle.reset("f1");
        // Sent at once although the interval has not elapsed, and without the
        // held sample's duration: both belonged to the playback that ended.
        assert_eq!(
            throttle.decide("f1", 5.0, None, false),
            ThrottleDecision::Send {
                position_secs: 5.0,
                duration_secs: None,
            },
            "the held 11s sample should have been discarded"
        );
    }
}
