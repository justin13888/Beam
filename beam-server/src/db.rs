//! Database connection bootstrap: pool sizing and startup retry.

use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use tracing::warn;

use crate::config::ServerConfig;

/// Retries beyond the first attempt. With [`backoff_delay`]'s schedule the
/// last retry fires ~2.5 minutes in -- comfortably past a Postgres container
/// finishing bringup when compose starts it alongside this process.
const MAX_RETRIES: u32 = 10;

/// Per-attempt and pool-lifecycle timeouts are fixed; only the pool size is
/// operator-tunable (`BEAM_DB_MAX_CONNECTIONS` / `BEAM_DB_MIN_CONNECTIONS`).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(8);
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_LIFETIME: Duration = Duration::from_secs(1800);

/// Connects to Postgres with tuned pool options, retrying with exponential
/// backoff so the server survives the database coming up after it or a
/// transient network blip at startup.
pub async fn connect(config: &ServerConfig) -> Result<DatabaseConnection, DbErr> {
    let options = connect_options(config);
    let clock = beam_domain::services::RealClock;
    retrying(&clock, || {
        let options = options.clone();
        async move { Database::connect(options).await }
    })
    .await
}

/// Retry `attempt` with the module's backoff schedule until it succeeds or the
/// budget is spent.
///
/// Generic over the operation and the clock so the retry *policy* -- how many
/// attempts, how long between them, and that the last error is the one
/// returned -- is testable. Driving it through `Database::connect` would need
/// a database that is reachable on the fourth try and not before.
async fn retrying<T, F, Fut>(
    clock: &dyn beam_domain::services::Clock,
    mut attempt_fn: F,
) -> Result<T, DbErr>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DbErr>>,
{
    let mut attempt = 0u32;
    loop {
        match attempt_fn().await {
            Ok(value) => return Ok(value),
            Err(e) if attempt < MAX_RETRIES => {
                let delay = backoff_delay(attempt);
                warn!(
                    "Database connection attempt {}/{} failed ({e}); retrying in {}s",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    delay.as_secs()
                );
                clock.sleep(delay).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Pool options for `config`: sizes from config, timeouts fixed. SQLx
/// per-query logging is disabled -- it logs full statements at debug level,
/// which is noise in production and a leak risk for parameter values.
fn connect_options(config: &ServerConfig) -> ConnectOptions {
    let mut options = ConnectOptions::new(&config.database_url);
    options
        .max_connections(config.db_max_connections)
        .min_connections(config.db_min_connections)
        .connect_timeout(CONNECT_TIMEOUT)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(IDLE_TIMEOUT)
        .max_lifetime(MAX_LIFETIME)
        .sqlx_logging(false);
    options
}

/// Delay before retry number `attempt` (0-based): 1s, 2s, 4s, ... capped at
/// 30s per attempt.
fn backoff_delay(attempt: u32) -> Duration {
    const CAP_SECS: u64 = 30;
    let secs = 1u64.checked_shl(attempt).unwrap_or(CAP_SECS).min(CAP_SECS);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number of attempts `retrying` made, and the delays it waited.
    async fn drive_retries(
        outcomes: Vec<Result<u32, DbErr>>,
    ) -> (Result<u32, DbErr>, usize, Vec<u64>) {
        use std::sync::Mutex;

        let clock = beam_domain::services::TestClock::new();
        let remaining = Mutex::new(outcomes.into_iter());
        let calls = std::sync::atomic::AtomicUsize::new(0);

        // The `TestClock` resolves a sleep only when advanced, so drive the
        // retry loop on one task and advance from another as it parks.
        let clock = std::sync::Arc::new(clock);
        let driver = {
            let clock = clock.clone();
            tokio::spawn(async move {
                loop {
                    if clock.waiter_count() > 0 {
                        clock.advance(std::time::Duration::from_secs(60));
                    }
                    tokio::task::yield_now().await;
                }
            })
        };

        let result = retrying(clock.as_ref(), || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Succeeds once the queue is spent, so a `retrying` whose bound
            // has been removed terminates and fails an assertion instead of
            // hanging until the harness times out.
            let next = remaining.lock().unwrap().next().unwrap_or(Ok(0));
            async move { next }
        })
        .await;

        driver.abort();
        let attempts = calls.load(std::sync::atomic::Ordering::SeqCst);
        (
            result,
            attempts,
            (0..MAX_RETRIES)
                .map(|a| backoff_delay(a).as_secs())
                .collect(),
        )
    }

    #[tokio::test]
    async fn a_connection_that_succeeds_first_time_is_not_retried() {
        let (result, attempts, _) = drive_retries(vec![Ok(7)]).await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts, 1, "a working database must not be slept on");
    }

    #[tokio::test]
    async fn a_database_that_comes_up_late_is_waited_for() {
        // The whole point of the retry: compose starts Postgres alongside this
        // process, so the first few attempts are expected to fail.
        let (result, attempts, _) = drive_retries(vec![
            Err(DbErr::Custom("refused".to_string())),
            Err(DbErr::Custom("refused".to_string())),
            Ok(7),
        ])
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn the_retry_budget_is_bounded_and_the_last_error_is_returned() {
        // Unbounded retry would hang startup forever against a database that
        // is never coming back, with no error and no exit.
        // More failures queued than the budget allows, then success: an
        // unbounded loop would reach the success and be caught by the
        // assertions below rather than hanging.
        let (result, attempts, _) = drive_retries(
            (0..(MAX_RETRIES + 5))
                .map(|i| Err(DbErr::Custom(format!("refused {i}"))))
                .collect(),
        )
        .await;

        assert_eq!(
            attempts,
            MAX_RETRIES as usize + 1,
            "one initial attempt plus MAX_RETRIES retries, and no more"
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            DbErr::Custom(format!("refused {MAX_RETRIES}")).to_string(),
            "the error the caller sees is the last failure, not the first"
        );
    }

    #[test]
    fn backoff_doubles_then_caps_at_thirty_seconds() {
        let schedule: Vec<u64> = (0..MAX_RETRIES)
            .map(|attempt| backoff_delay(attempt).as_secs())
            .collect();
        assert_eq!(schedule, vec![1, 2, 4, 8, 16, 30, 30, 30, 30, 30]);
        // Absurdly high attempt numbers must not overflow the shift.
        assert_eq!(backoff_delay(u32::MAX).as_secs(), 30);
    }

    #[test]
    fn connect_options_apply_configured_pool_sizes() {
        let config = ServerConfig {
            video_dir: std::path::PathBuf::from("/tmp"),
            data_dir: std::path::PathBuf::from("/tmp"),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            db_max_connections: 42,
            db_min_connections: 7,
            watch_enabled: false,
            anilist_enabled: false,
            ..Default::default()
        };
        let options = connect_options(&config);
        assert_eq!(options.get_max_connections(), Some(42));
        assert_eq!(options.get_min_connections(), Some(7));
        assert!(!options.get_sqlx_logging());
    }
}
