//! Time, injected.
//!
//! The progress throttle and the retry queue are both time-dependent, and a
//! test that proves "nothing is sent before fifteen seconds elapse" must not
//! take fifteen seconds to run. The same trait-and-fake shape `beam-server`
//! already uses for its rate limiter.

/// A source of wall-clock time.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Seconds since the Unix epoch.
    ///
    /// Wall clock rather than a monotonic instant, because the retry queue is
    /// persisted and compared across process restarts, where a monotonic
    /// instant is meaningless.
    fn now_unix(&self) -> i64;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use test_clock::TestClock;

#[cfg(any(test, feature = "test-utils"))]
mod test_clock {
    use super::Clock;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A clock a test moves by hand.
    #[derive(Debug)]
    pub struct TestClock {
        now: AtomicI64,
    }

    impl TestClock {
        /// Start at `now_unix` seconds past the epoch.
        #[must_use]
        pub fn new(now_unix: i64) -> Self {
            Self {
                now: AtomicI64::new(now_unix),
            }
        }

        /// Move time forward.
        pub fn advance_secs(&self, seconds: i64) {
            self.now.fetch_add(seconds, Ordering::SeqCst);
        }
    }

    impl Default for TestClock {
        fn default() -> Self {
            Self::new(1_700_000_000)
        }
    }

    impl Clock for TestClock {
        fn now_unix(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_clock_only_moves_when_told_to() {
        let clock = TestClock::new(1_000);
        assert_eq!(clock.now_unix(), 1_000);
        assert_eq!(clock.now_unix(), 1_000);
        clock.advance_secs(15);
        assert_eq!(clock.now_unix(), 1_015);
    }

    #[test]
    fn the_system_clock_returns_a_plausible_epoch_time() {
        // Sanity only: the point is that it is not zero or negative.
        assert!(SystemClock.now_unix() > 1_700_000_000);
    }
}
