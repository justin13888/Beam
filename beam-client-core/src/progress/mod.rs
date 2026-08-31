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
    /// Held back by the throttle. The position is remembered, so the next
    /// eligible report carries the newest value rather than this one.
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

/// A pending position for one file, superseded by any newer one.
#[derive(Debug, Clone, Copy)]
struct Pending {
    position_secs: f64,
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
        /// The position to send, which may be newer than the one offered if a
        /// throttled sample was coalesced in the meantime.
        position_secs: f64,
        /// The duration to send alongside it.
        duration_secs: Option<f64>,
    },
    /// Hold back; the position has been remembered.
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
            // A coalesced sample may be newer than the one just offered; the
            // throttle exists to reduce request count, not to lose progress.
            let coalesced = self.pending.lock().expect("pending lock").remove(file_id);
            let (position_secs, duration_secs) = match coalesced {
                Some(held) if held.position_secs > position_secs => {
                    (held.position_secs, held.duration_secs.or(duration_secs))
                }
                _ => (position_secs, duration_secs),
            };
            self.last_sent
                .lock()
                .expect("last_sent lock")
                .insert(file_id.to_owned(), now);
            return ThrottleDecision::Send {
                position_secs,
                duration_secs,
            };
        }

        self.pending.lock().expect("pending lock").insert(
            file_id.to_owned(),
            Pending {
                position_secs,
                duration_secs,
            },
        );
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
    fn a_held_position_is_coalesced_into_the_next_send() {
        // The throttle exists to cut request count, not to lose progress: the
        // newest position held back must be the one that eventually lands.
        let clock = Arc::new(TestClock::new(1_000));
        let throttle = throttle(&clock);
        throttle.decide("f1", 10.0, Some(7200.0), false);

        clock.advance_secs(5);
        throttle.decide("f1", 15.0, Some(7200.0), false);
        clock.advance_secs(5);
        throttle.decide("f1", 20.0, Some(7200.0), false);

        clock.advance_secs(5);
        // The caller offers a stale position; the coalesced newer one wins.
        match throttle.decide("f1", 12.0, None, false) {
            ThrottleDecision::Send { position_secs, .. } => {
                assert!((position_secs - 20.0).abs() < f64::EPSILON);
            }
            other => panic!("expected a send, got {other:?}"),
        }
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
        throttle.decide("f1", 11.0, None, false);

        throttle.reset("f1");
        match throttle.decide("f1", 5.0, None, false) {
            ThrottleDecision::Send { position_secs, .. } => {
                assert!(
                    (position_secs - 5.0).abs() < f64::EPSILON,
                    "the held 11s sample should have been discarded"
                );
            }
            other => panic!("expected a send, got {other:?}"),
        }
    }
}
