//! Clock abstraction for deterministic, offline testing of time-based logic.
//!
//! Production code uses [`RealClock`]; tests use [`TestClock`], whose
//! [`TestClock::advance`] resolves due `sleep` calls without any wall-clock wait.
//!
//! One trait covers both time bases the codebase needs. Wall-clock
//! [`Clock::now`] stamps persisted rows and drives the rescan cadence;
//! monotonic [`Clock::monotonic`] measures elapsed intervals for the rate
//! limiter and uptime, where a wall-clock jump must not be observable. Keeping
//! them on one trait means one `TestClock` and one `advance`, so a test that
//! moves time moves it consistently for both.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

/// A source of time. Abstracted behind a trait so time-dependent logic can be
/// driven deterministically in tests.
#[async_trait::async_trait]
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current wall-clock time.
    fn now(&self) -> DateTime<Utc>;

    /// The current monotonic instant. Never moves backwards, and is unaffected
    /// by wall-clock adjustments.
    fn monotonic(&self) -> Instant;

    /// Sleep for `duration`. Resolves immediately when `duration` is zero.
    async fn sleep(&self, duration: Duration);
}

/// Production clock backed by the system clock and `tokio::time`.
#[derive(Debug, Default, Clone)]
pub struct RealClock;

#[async_trait::async_trait]
impl Clock for RealClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn monotonic(&self) -> Instant {
        Instant::now()
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// Test clock whose time advances only via [`TestClock::advance`]. Pending
    /// [`Clock::sleep`] calls resolve when `advance` pushes the clock past their
    /// deadline, so time-based logic runs with zero real delay.
    ///
    /// Wall-clock and monotonic time advance together: `monotonic()` is a fixed
    /// base instant captured at construction plus the total advanced duration.
    #[derive(Debug)]
    pub struct TestClock {
        base: Instant,
        state: std::sync::Mutex<TestClockState>,
    }

    #[derive(Debug)]
    struct TestClockState {
        now: DateTime<Utc>,
        elapsed: Duration,
        waiters: Vec<Waiter>,
    }

    #[derive(Debug)]
    struct Waiter {
        deadline: DateTime<Utc>,
        sender: tokio::sync::oneshot::Sender<()>,
    }

    impl TestClock {
        /// Create a clock fixed at the Unix epoch.
        pub fn new() -> Self {
            Self::starting_at(DateTime::from_timestamp(0, 0).expect("unix epoch is valid"))
        }

        /// Create a clock fixed at a specific time.
        pub fn starting_at(now: DateTime<Utc>) -> Self {
            Self {
                base: Instant::now(),
                state: std::sync::Mutex::new(TestClockState {
                    now,
                    elapsed: Duration::ZERO,
                    waiters: Vec::new(),
                }),
            }
        }

        /// Advance the clock, resolving every `sleep` whose deadline is now reached.
        pub fn advance(&self, duration: Duration) {
            let delta = chrono::Duration::from_std(duration).expect("duration within range");
            let mut state = self.state.lock().unwrap();
            state.now += delta;
            state.elapsed += duration;
            let now = state.now;
            let mut pending = Vec::new();
            for waiter in state.waiters.drain(..) {
                if waiter.deadline <= now {
                    let _ = waiter.sender.send(());
                } else {
                    pending.push(waiter);
                }
            }
            state.waiters = pending;
        }

        /// Number of `sleep` calls currently awaiting. Lets tests wait until the
        /// code under test has registered before calling `advance`.
        pub fn waiter_count(&self) -> usize {
            self.state.lock().unwrap().waiters.len()
        }
    }

    impl Default for TestClock {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            self.state.lock().unwrap().now
        }

        fn monotonic(&self) -> Instant {
            self.base + self.state.lock().unwrap().elapsed
        }

        async fn sleep(&self, duration: Duration) {
            if duration.is_zero() {
                return;
            }
            let receiver = {
                let mut state = self.state.lock().unwrap();
                let deadline = state.now
                    + chrono::Duration::from_std(duration).expect("duration within range");
                let (sender, receiver) = tokio::sync::oneshot::channel();
                state.waiters.push(Waiter { deadline, sender });
                receiver
            };
            // A closed channel (TestClock dropped) is treated as the sleep waking.
            let _ = receiver.await;
        }
    }
}

// Re-exported at the module root so `TestClock` keeps the path it had before the
// doubles moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::TestClock;
