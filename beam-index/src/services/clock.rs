//! Clock abstraction for deterministic, offline testing of time-based logic.
//!
//! Production code uses [`RealClock`]; tests use [`TestClock`], whose
//! [`TestClock::advance`] resolves due `sleep` calls without any wall-clock wait.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// A source of time. Abstracted behind a trait so the periodic-rescan loop can
/// be driven deterministically in tests.
#[async_trait::async_trait]
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current wall-clock time.
    fn now(&self) -> DateTime<Utc>;
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

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Test clock whose time advances only via [`TestClock::advance`]. Pending
/// [`Clock::sleep`] calls resolve when `advance` pushes the clock past their
/// deadline, so time-based logic runs with zero real delay.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
pub struct TestClock {
    state: std::sync::Mutex<TestClockState>,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
struct TestClockState {
    now: DateTime<Utc>,
    waiters: Vec<Waiter>,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
struct Waiter {
    deadline: DateTime<Utc>,
    sender: tokio::sync::oneshot::Sender<()>,
}

#[cfg(any(test, feature = "test-utils"))]
impl TestClock {
    /// Create a clock fixed at the Unix epoch.
    pub fn new() -> Self {
        Self::starting_at(DateTime::from_timestamp(0, 0).expect("unix epoch is valid"))
    }

    /// Create a clock fixed at a specific time.
    pub fn starting_at(now: DateTime<Utc>) -> Self {
        Self {
            state: std::sync::Mutex::new(TestClockState {
                now,
                waiters: Vec::new(),
            }),
        }
    }

    /// Advance the clock, resolving every `sleep` whose deadline is now reached.
    pub fn advance(&self, duration: Duration) {
        let delta = chrono::Duration::from_std(duration).expect("duration within range");
        let mut state = self.state.lock().unwrap();
        state.now += delta;
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

#[cfg(any(test, feature = "test-utils"))]
impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        self.state.lock().unwrap().now
    }

    async fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let receiver = {
            let mut state = self.state.lock().unwrap();
            let deadline =
                state.now + chrono::Duration::from_std(duration).expect("duration within range");
            let (sender, receiver) = tokio::sync::oneshot::channel();
            state.waiters.push(Waiter { deadline, sender });
            receiver
        };
        // A closed channel (TestClock dropped) is treated as the sleep waking.
        let _ = receiver.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_now_advances_with_advance() {
        let clock = TestClock::new();
        let start = clock.now();
        clock.advance(Duration::from_secs(60));
        assert_eq!(clock.now(), start + chrono::Duration::seconds(60));
    }

    #[tokio::test]
    async fn test_zero_duration_sleep_returns_immediately() {
        let clock = TestClock::new();
        clock.sleep(Duration::ZERO).await; // must not hang
    }

    #[tokio::test]
    async fn test_sleep_resolves_after_advance() {
        let clock = Arc::new(TestClock::new());
        let task = {
            let clock = clock.clone();
            tokio::spawn(async move { clock.sleep(Duration::from_secs(30)).await })
        };

        while clock.waiter_count() == 0 {
            tokio::task::yield_now().await;
        }
        clock.advance(Duration::from_secs(30));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn test_advance_fires_only_due_waiters() {
        let clock = Arc::new(TestClock::new());
        let short = {
            let clock = clock.clone();
            tokio::spawn(async move { clock.sleep(Duration::from_secs(10)).await })
        };
        let long = {
            let clock = clock.clone();
            tokio::spawn(async move { clock.sleep(Duration::from_secs(100)).await })
        };

        while clock.waiter_count() < 2 {
            tokio::task::yield_now().await;
        }

        clock.advance(Duration::from_secs(10));
        short.await.unwrap();
        assert!(
            !long.is_finished(),
            "the 100s sleeper must still be pending"
        );

        clock.advance(Duration::from_secs(90));
        long.await.unwrap();
    }
}
