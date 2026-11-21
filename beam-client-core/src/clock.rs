//! Time, injected.
//!
//! The progress throttle and the retry queue are both time-dependent, and a
//! test that proves "nothing is sent before fifteen seconds elapse" must not
//! take fifteen seconds to run. The same trait-and-fake shape `beam-server`
//! already uses for its rate limiter.
//!
//! Waiting is on the same seam: the device-login poll loop paces itself by
//! [`Clock::sleep_secs`], so a test asserts every wait it asked for -- and the
//! outcome -- without a real second passing.

/// A source of wall-clock time, and of waiting on it.
#[async_trait::async_trait]
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Seconds since the Unix epoch.
    ///
    /// Wall clock rather than a monotonic instant, because the retry queue is
    /// persisted and compared across process restarts, where a monotonic
    /// instant is meaningless.
    fn now_unix(&self) -> i64;

    /// Wait `seconds` before resolving.
    ///
    /// Cancelled by dropping the future: nothing is left running once the
    /// caller gives up.
    async fn sleep_secs(&self, seconds: u64);
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[async_trait::async_trait]
impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    /// A tokio timer. Every foreign async call runs inside the tokio context
    /// uniffi's `async_runtime = "tokio"` provides, which is what drives it.
    async fn sleep_secs(&self, seconds: u64) {
        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    }
}

#[cfg(test)]
pub use test_clock::TestClock;

#[mutants::skip]
#[cfg(test)]
mod test_clock {
    use super::Clock;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A clock a test moves by hand.
    ///
    /// A sleep returns at once, having moved the clock forward by its length
    /// and recorded it, so a test reads back every wait the code asked for.
    #[derive(Debug)]
    pub struct TestClock {
        now: AtomicI64,
        sleeps: Mutex<Vec<u64>>,
    }

    impl TestClock {
        /// Start at `now_unix` seconds past the epoch.
        #[must_use]
        pub fn new(now_unix: i64) -> Self {
            Self {
                now: AtomicI64::new(now_unix),
                sleeps: Mutex::new(Vec::new()),
            }
        }

        /// Move time forward.
        pub fn advance_secs(&self, seconds: i64) {
            self.now.fetch_add(seconds, Ordering::SeqCst);
        }

        /// Every sleep asked for so far, in seconds, oldest first.
        #[must_use]
        pub fn sleeps(&self) -> Vec<u64> {
            self.sleeps.lock().expect("sleeps lock").clone()
        }
    }

    impl Default for TestClock {
        fn default() -> Self {
            Self::new(1_700_000_000)
        }
    }

    #[async_trait::async_trait]
    impl Clock for TestClock {
        fn now_unix(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }

        async fn sleep_secs(&self, seconds: u64) {
            self.sleeps.lock().expect("sleeps lock").push(seconds);
            self.advance_secs(i64::try_from(seconds).unwrap_or(i64::MAX));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, SystemClock};

    /// The real clock reads the present, in seconds: after the day this test
    /// was written, and within a few seconds of the standard library's own
    /// reading. A clock stuck at the epoch would park every queued progress
    /// sample's back-off decades in the past.
    #[test]
    fn the_system_clock_reads_the_present_in_seconds() {
        let now = SystemClock.now_unix();
        assert!(now > 1_793_232_000, "{now} is before 2026-10-29");
        let reference = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs();
        let reference = i64::try_from(reference).expect("fits");
        assert!((reference - now).abs() <= 5, "{now} vs {reference}");
    }

    /// The real clock's sleep is a tokio timer of exactly that many seconds:
    /// one that returned at once would turn the device-login loop into a
    /// tight poll against the server. Paused time, so nothing really waits.
    #[tokio::test(start_paused = true)]
    async fn the_system_clock_sleeps_on_the_tokio_timer() {
        let before = tokio::time::Instant::now();
        SystemClock.sleep_secs(7).await;
        let waited = before.elapsed();
        assert!(
            waited >= std::time::Duration::from_secs(7)
                && waited < std::time::Duration::from_secs(8),
            "{waited:?}"
        );
    }
}
