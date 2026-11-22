//! Database connection bootstrap: pool sizing and startup retry, and the
//! pool's metrics ([#458](https://github.com/justin13888/beam/issues/458)).
//!
//! Every ranged media request costs several database round trips, so a
//! saturated pool shows up as player rebuffering. Two things make it visible:
//!
//! * **Failed queries.** [`connect`] hands the connection [`record_query`],
//!   which counts every statement that failed as
//!   `beam_db_errors_total{kind="query"}`.
//! * **Pool samples.** [`spawn_pool_sampler`] reads the pool's open and idle
//!   connections and its maximum every [`POOL_SAMPLE_INTERVAL`], and times
//!   acquiring one connection. Neither sea-orm nor SQLx reports how long a
//!   request waited for a connection, so the probe waits in the same queue
//!   instead: when the pool is saturated the probe waits as long as a request
//!   would, and when the wait exceeds the acquire timeout the probe's failure
//!   is counted.

use std::sync::Arc;
use std::time::Duration;

use beam_domain::services::Clock;
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use tracing::warn;

use crate::config::ServerConfig;

/// How often [`spawn_pool_sampler`] samples the pool.
///
/// The interval `crate::metrics::spawn_upkeep` drains histograms at, so a
/// scrape interval of 15 seconds or more always sees a fresh sample.
pub const POOL_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

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
    let mut db = retrying(&clock, || {
        let options = options.clone();
        async move { Database::connect(options).await }
    })
    .await?;
    db.set_metric_callback(record_query);
    Ok(db)
}

/// Count a statement that failed as `beam_db_errors_total{kind="query"}`.
///
/// sea-orm calls this after every statement it runs on the pool. It sees only
/// whether the statement failed, not why, so every failure is one kind. A
/// connection that could not be acquired fails before the statement runs and
/// never reaches here; the pool sampler counts that.
pub fn record_query(info: &sea_orm::metric::Info<'_>) {
    if info.failed {
        metrics::counter!("beam_db_errors_total", "kind" => "query").increment(1);
    }
}

/// The pool's connections at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolConnections {
    /// Connections open, idle or in use.
    pub open: u32,
    /// Open connections nobody holds.
    pub idle: u32,
    /// The most connections the pool may open.
    pub max: u32,
}

/// Why a probe could not acquire a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireFailure {
    /// No connection came free within the acquire timeout: the pool is
    /// saturated.
    TimedOut,
    /// The pool was closed.
    Closed,
    /// A new connection was needed and could not be opened.
    Connect,
}

impl AcquireFailure {
    /// The `kind` label of `beam_db_errors_total`.
    fn kind(self) -> &'static str {
        match self {
            Self::TimedOut => "acquire_timeout",
            Self::Closed => "pool_closed",
            Self::Connect => "connect",
        }
    }
}

/// A connection pool the sampler can read and probe.
///
/// The seam [`sample_pool`] runs against, so what it records is tested without
/// a database; the implementation is SQLx's Postgres pool.
pub trait SampledPool: Send + Sync + 'static {
    /// The pool's connections now.
    fn connections(&self) -> PoolConnections;

    /// Acquire one connection and give it straight back.
    fn probe_acquire(&self) -> impl Future<Output = Result<(), AcquireFailure>> + Send;
}

impl SampledPool for sea_orm::sqlx::PgPool {
    fn connections(&self) -> PoolConnections {
        PoolConnections {
            open: self.size(),
            idle: u32::try_from(self.num_idle()).unwrap_or(u32::MAX),
            max: self.options().get_max_connections(),
        }
    }

    async fn probe_acquire(&self) -> Result<(), AcquireFailure> {
        match self.acquire().await {
            Ok(connection) => {
                drop(connection);
                Ok(())
            }
            Err(sea_orm::sqlx::Error::PoolTimedOut) => Err(AcquireFailure::TimedOut),
            Err(sea_orm::sqlx::Error::PoolClosed) => Err(AcquireFailure::Closed),
            Err(_) => Err(AcquireFailure::Connect),
        }
    }
}

/// Record one sample of `pool`: its connections by state, its maximum, and
/// how long acquiring a connection took, measured on `clock`.
pub async fn sample_pool(pool: &impl SampledPool, clock: &dyn Clock) {
    let PoolConnections { open, idle, max } = pool.connections();
    metrics::gauge!("beam_db_pool_connections", "state" => "idle").set(f64::from(idle));
    metrics::gauge!("beam_db_pool_connections", "state" => "in_use")
        .set(f64::from(open.saturating_sub(idle)));
    metrics::gauge!("beam_db_pool_max").set(f64::from(max));

    let started = clock.monotonic();
    let acquired = pool.probe_acquire().await;
    let waited = clock.monotonic().saturating_duration_since(started);
    metrics::histogram!("beam_db_acquire_duration_seconds").record(waited.as_secs_f64());

    if let Err(failure) = acquired {
        metrics::counter!("beam_db_errors_total", "kind" => failure.kind()).increment(1);
    }
}

/// Sample `pool` every [`POOL_SAMPLE_INTERVAL`] for as long as the process
/// runs, starting at once.
pub fn spawn_pool_sampler(
    pool: impl SampledPool,
    clock: Arc<dyn Clock>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POOL_SAMPLE_INTERVAL);
        // A probe that waited out the acquire timeout must not be followed by
        // a burst of catch-up probes into the same saturated pool.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            sample_pool(&pool, clock.as_ref()).await;
        }
    })
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

    // ─── pool metrics ───────────────────────────────────────────────────────

    use crate::routes::test_support::Recorded;
    use beam_domain::services::TestClock;
    use metrics_util::debugging::DebuggingRecorder;

    /// A pool with fixed connections whose probe takes `wait` on `clock` and
    /// then answers `outcome`.
    struct ScriptedPool {
        connections: PoolConnections,
        clock: Arc<TestClock>,
        wait: Duration,
        outcome: Result<(), AcquireFailure>,
    }

    impl SampledPool for ScriptedPool {
        fn connections(&self) -> PoolConnections {
            self.connections
        }

        async fn probe_acquire(&self) -> Result<(), AcquireFailure> {
            self.clock.advance(self.wait);
            self.outcome
        }
    }

    /// What one [`sample_pool`] of `pool` records.
    async fn sampled(pool: ScriptedPool) -> Recorded {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        {
            // Thread-local, and a `#[tokio::test]` runs on one thread.
            let _local = metrics::set_default_local_recorder(&recorder);
            sample_pool(&pool, pool.clock.as_ref()).await;
        }
        Recorded::of(&snapshotter)
    }

    #[tokio::test]
    async fn a_pool_sample_splits_open_connections_into_idle_and_in_use() {
        let recorded = sampled(ScriptedPool {
            connections: PoolConnections {
                open: 7,
                idle: 2,
                max: 10,
            },
            clock: Arc::new(TestClock::new()),
            wait: Duration::from_millis(250),
            outcome: Ok(()),
        })
        .await;

        assert_eq!(
            recorded.gauge("beam_db_pool_connections", &[("state", "idle")]),
            Some(2.0)
        );
        assert_eq!(
            recorded.gauge("beam_db_pool_connections", &[("state", "in_use")]),
            Some(5.0),
            "in use is the open connections nobody left idle"
        );
        assert_eq!(recorded.gauge("beam_db_pool_max", &[]), Some(10.0));
        assert_eq!(
            recorded.histogram("beam_db_acquire_duration_seconds", &[]),
            Some(vec![0.25]),
            "the probe's wait is measured on the injected clock"
        );
        for kind in ["acquire_timeout", "pool_closed", "connect", "query"] {
            assert_eq!(
                recorded.counter("beam_db_errors_total", &[("kind", kind)]),
                None,
                "a probe that acquired is no error"
            );
        }
    }

    #[tokio::test]
    async fn a_probe_that_cannot_acquire_is_counted_by_why_and_still_timed() {
        for (failure, kind) in [
            (AcquireFailure::TimedOut, "acquire_timeout"),
            (AcquireFailure::Closed, "pool_closed"),
            (AcquireFailure::Connect, "connect"),
        ] {
            let recorded = sampled(ScriptedPool {
                connections: PoolConnections {
                    open: 10,
                    idle: 0,
                    max: 10,
                },
                clock: Arc::new(TestClock::new()),
                wait: ACQUIRE_TIMEOUT,
                outcome: Err(failure),
            })
            .await;

            assert_eq!(
                recorded.counter("beam_db_errors_total", &[("kind", kind)]),
                Some(1),
                "{failure:?} is counted as {kind}"
            );
            assert_eq!(
                recorded.histogram("beam_db_acquire_duration_seconds", &[]),
                Some(vec![ACQUIRE_TIMEOUT.as_secs_f64()]),
                "a saturated pool's wait is the very thing to see"
            );
        }
    }

    #[test]
    fn only_a_failed_statement_is_counted_as_a_query_error() {
        let statement =
            sea_orm::Statement::from_string(sea_orm::DbBackend::Postgres, "SELECT 1".to_owned());
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        metrics::with_local_recorder(&recorder, || {
            for failed in [false, true, false] {
                record_query(&sea_orm::metric::Info {
                    elapsed: Duration::from_millis(3),
                    statement: &statement,
                    failed,
                });
            }
        });

        assert_eq!(
            Recorded::of(&snapshotter).counter("beam_db_errors_total", &[("kind", "query")]),
            Some(1)
        );
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
