//! Telling a connection that cannot keep up from a stall that is only noise.
//!
//! Beam has no adaptive ladder to fall back on ([ADR-0014]), so the only
//! answer to a slow connection is a discrete switch to a smaller file that
//! already exists. That switch interrupts playback and costs the viewer
//! picture quality, so it must not fire on a single hiccup: one stall while a
//! phone moves between cells is noise, and downgrading on it would trade a
//! two-second pause for an hour of worse picture.
//!
//! What counts as *sustained* is decided here, once, rather than per
//! platform. The detector is a pure state machine over monotonic timestamps
//! the platform supplies, so it holds no clock of its own and a test can walk
//! it through a minute of playback without waiting a minute.
//!
//! The platform reports only mid-playback stalls. Buffering at start-up,
//! after a seek, or after a source switch is the player filling an empty
//! buffer on purpose, not the connection falling behind, and counting it would
//! make every seek look like a symptom.
//!
//! [ADR-0014]: ../../../docs/architecture/decisions/ADR-0014-adaptive-streaming-rejected.md

use std::collections::VecDeque;

/// How far back the detector looks, in milliseconds.
pub const WINDOW_MS: u64 = 60_000;

/// Stalls beginning within [`WINDOW_MS`] that make rebuffering sustained.
///
/// Three, because two can be one bad handover followed by the buffer refilling
/// at the edge of what the link can carry; a third within the same minute
/// means the link is not carrying the file.
pub const STALLS_TO_TRIP: usize = 3;

/// Time spent stalled within [`WINDOW_MS`] that makes rebuffering sustained,
/// in milliseconds, whether in one stall or several.
///
/// A player resumes once it has a few seconds buffered (ExoPlayer's default is
/// five), so a stall lasting fifteen seconds means the link is delivering
/// media at a third of real time or less: no amount of waiting fixes that.
pub const STALLED_MS_TO_TRIP: u64 = 15_000;

/// One mid-playback stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stall {
    started_ms: u64,
    /// `None` while the stall is still in progress.
    ended_ms: Option<u64>,
}

/// Whether the current file is rebuffering persistently enough to abandon.
///
/// Once tripped it stays tripped until [`RebufferDetector::reset`]: the
/// verdict is about the file being played, and the platform resets it when a
/// different file starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebufferDetector {
    stalls: VecDeque<Stall>,
    sustained: bool,
}

impl RebufferDetector {
    /// A detector that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Playback stalled waiting for data at `now_ms`.
    ///
    /// A second start while a stall is already open is the same stall reported
    /// twice, and is ignored rather than counted again.
    pub fn stall_started(&mut self, now_ms: u64) -> bool {
        let already_open = self
            .stalls
            .back()
            .is_some_and(|stall| stall.ended_ms.is_none());
        if !already_open {
            self.stalls.push_back(Stall {
                started_ms: now_ms,
                ended_ms: None,
            });
        }
        self.evaluate(now_ms)
    }

    /// Playback resumed at `now_ms`. Without an open stall this changes
    /// nothing.
    pub fn stall_ended(&mut self, now_ms: u64) -> bool {
        if let Some(stall) = self.stalls.back_mut()
            && stall.ended_ms.is_none()
        {
            // A clock that reads earlier than the stall's start would make the
            // stall negative; it is held at zero length instead.
            stall.ended_ms = Some(now_ms.max(stall.started_ms));
        }
        self.evaluate(now_ms)
    }

    /// Re-judge at `now_ms` without a new event.
    ///
    /// Needed because a single stall that never ends produces no further
    /// events at all, and it is exactly the case that most needs to trip.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        self.evaluate(now_ms)
    }

    /// Whether rebuffering has been judged sustained. Tests only: every
    /// production caller reads the verdict each event returns.
    #[cfg(test)]
    const fn is_sustained(&self) -> bool {
        self.sustained
    }

    /// Forget everything, because a different file is now playing.
    pub fn reset(&mut self) {
        self.stalls.clear();
        self.sustained = false;
    }

    fn evaluate(&mut self, now_ms: u64) -> bool {
        if self.sustained {
            return true;
        }
        let window_start = now_ms.saturating_sub(WINDOW_MS);
        // A stall that finished before the window opened can never count
        // again; dropping it keeps the history bounded over a long film.
        self.stalls
            .retain(|stall| stall.ended_ms.is_none_or(|ended| ended > window_start));

        let started_in_window = self
            .stalls
            .iter()
            .filter(|stall| stall.started_ms >= window_start)
            .count();
        let stalled_ms: u64 = self
            .stalls
            .iter()
            .map(|stall| {
                let from = stall.started_ms.max(window_start);
                let to = stall.ended_ms.unwrap_or(now_ms).min(now_ms);
                to.saturating_sub(from)
            })
            .sum();

        self.sustained = started_in_window >= STALLS_TO_TRIP || stalled_ms >= STALLED_MS_TO_TRIP;
        self.sustained
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stall of `length_ms` beginning at `at_ms`, reported start and end.
    fn stall(detector: &mut RebufferDetector, at_ms: u64, length_ms: u64) -> bool {
        detector.stall_started(at_ms);
        detector.stall_ended(at_ms + length_ms)
    }

    #[test]
    fn a_single_short_stall_is_noise() {
        let mut detector = RebufferDetector::new();
        assert!(!stall(&mut detector, 10_000, 3_000));
        assert!(!detector.poll(30_000));
        assert!(!detector.is_sustained());
    }

    #[test]
    fn two_stalls_in_a_minute_are_still_noise() {
        let mut detector = RebufferDetector::new();
        assert!(!stall(&mut detector, 0, 2_000));
        assert!(!stall(&mut detector, 20_000, 2_000));
    }

    /// The window is the last minute, open at its start: a stall that was
    /// over by the instant the window opens is history, not one of its three.
    #[test]
    fn a_stall_over_as_the_window_opens_is_outside_it() {
        let mut detector = RebufferDetector::new();
        detector.stall_started(10_000);
        detector.stall_ended(10_000);
        stall(&mut detector, 40_000, 1_000);

        assert!(
            !detector.stall_started(10_000 + WINDOW_MS),
            "only two stalls began within the minute ending now"
        );
    }

    #[test]
    fn the_third_stall_within_a_minute_trips_it() {
        let mut detector = RebufferDetector::new();
        stall(&mut detector, 0, 1_000);
        stall(&mut detector, 20_000, 1_000);
        assert!(
            detector.stall_started(40_000),
            "the third stall trips on its start, not when it ends"
        );
    }

    #[test]
    fn stalls_spread_beyond_the_window_never_add_up() {
        // Three short stalls an hour of film apart are a healthy connection
        // having three bad moments, not a connection that cannot keep up.
        let mut detector = RebufferDetector::new();
        for minute in 0..10 {
            assert!(
                !stall(&mut detector, minute * 61_000, 2_000),
                "stall in minute {minute} tripped"
            );
        }
    }

    #[test]
    fn one_stall_that_will_not_end_trips_on_a_poll() {
        // No further event ever arrives for a stall that never ends, which is
        // why the platform polls.
        let mut detector = RebufferDetector::new();
        assert!(!detector.stall_started(5_000));
        assert!(!detector.poll(5_000 + STALLED_MS_TO_TRIP - 1));
        assert!(detector.poll(5_000 + STALLED_MS_TO_TRIP));
    }

    #[test]
    fn stalled_time_accumulates_across_two_stalls() {
        let mut detector = RebufferDetector::new();
        assert!(!stall(&mut detector, 0, 8_000));
        assert!(!detector.stall_started(30_000));
        assert!(!detector.poll(36_999));
        assert!(
            detector.poll(37_000),
            "8 s and then 7 s within a minute is 15 s stalled"
        );
    }

    #[test]
    fn only_the_part_of_a_stall_inside_the_window_counts() {
        // 10 s stalled from 0 s to 10 s; at 64 s the window opens at 4 s, so
        // only 6 s of it remain, and a further 8 s stall makes 14 s, short of
        // the threshold.
        let mut detector = RebufferDetector::new();
        stall(&mut detector, 0, 10_000);
        detector.stall_started(56_000);
        assert!(!detector.poll(64_000));
    }

    #[test]
    fn a_stall_reported_twice_counts_once() {
        let mut detector = RebufferDetector::new();
        detector.stall_started(0);
        detector.stall_started(500);
        detector.stall_ended(1_000);
        stall(&mut detector, 10_000, 1_000);
        assert!(
            !detector.is_sustained(),
            "a duplicated start must not become a third stall"
        );
    }

    #[test]
    fn an_end_without_a_start_changes_nothing() {
        let mut detector = RebufferDetector::new();
        assert!(!detector.stall_ended(1_000));
        assert_eq!(detector, RebufferDetector::new());
    }

    #[test]
    fn an_end_earlier_than_its_start_is_a_zero_length_stall() {
        let mut detector = RebufferDetector::new();
        detector.stall_started(20_000);
        assert!(!detector.stall_ended(0));
        assert!(!detector.poll(20_000 + STALLED_MS_TO_TRIP));
    }

    #[test]
    fn once_tripped_it_stays_tripped_until_reset() {
        let mut detector = RebufferDetector::new();
        detector.stall_started(0);
        assert!(detector.poll(STALLED_MS_TO_TRIP));
        detector.stall_ended(STALLED_MS_TO_TRIP + 1);
        assert!(
            detector.poll(STALLED_MS_TO_TRIP + 10 * WINDOW_MS),
            "the verdict is about the file, not about the last minute"
        );

        detector.reset();
        assert!(!detector.is_sustained());
        assert!(!detector.poll(STALLED_MS_TO_TRIP + 10 * WINDOW_MS));
    }

    #[test]
    fn a_long_film_keeps_a_bounded_history() {
        let mut detector = RebufferDetector::new();
        for minute in 0..1_000 {
            stall(&mut detector, minute * 120_000, 1_000);
        }
        assert!(detector.stalls.len() <= STALLS_TO_TRIP);
    }
}
