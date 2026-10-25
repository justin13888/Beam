//! Waiting on the stack, always for a named condition and never for long.
//!
//! A scan runs in the background and a watcher reconciles on its own
//! schedule, so some assertions can only be made once the server says it has
//! got there. Each wait polls one observable condition, gives up at a bound,
//! and says what it was waiting for when it does -- it never sleeps for an
//! ordering the server could have reported instead.

use std::future::Future;
use std::time::{Duration, Instant};

/// How often a condition is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Poll `probe` until it answers `Ok`, for at most `bound`. An `Err` is the
/// state it saw instead.
///
/// # Panics
///
/// When `bound` passes first, naming `what` and the last state `probe`
/// reported -- the message a failing run needs.
pub async fn until<T, F, Fut>(what: &str, bound: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let deadline = Instant::now() + bound;
    loop {
        let last = match probe().await {
            Ok(value) => return value,
            Err(state) => state,
        };
        assert!(
            Instant::now() < deadline,
            "gave up after {bound:?} waiting for {what}; last seen: {last}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
