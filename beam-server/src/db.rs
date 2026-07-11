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
    let mut attempt = 0u32;
    loop {
        match Database::connect(options.clone()).await {
            Ok(db) => return Ok(db),
            Err(e) if attempt < MAX_RETRIES => {
                let delay = backoff_delay(attempt);
                warn!(
                    "Database connection attempt {}/{} failed ({e}); retrying in {}s",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
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
            bind_address: "0.0.0.0:8000".to_string(),
            server_url: "http://localhost:8000".to_string(),
            enable_metrics: false,
            shutdown_timeout_secs: 30,
            video_dir: std::path::PathBuf::from("/tmp"),
            data_dir: std::path::PathBuf::from("/tmp"),
            database_url: "postgres://unused:unused@localhost/unused".to_string(),
            auto_migrate: true,
            db_max_connections: 42,
            db_min_connections: 7,
            hash_unknown_files: true,
            scan_interval_secs: 3600,
            watch_enabled: false,
            watch_debounce_ms: 2000,
            enrich_interval_secs: 300,
            enrich_batch_size: 25,
            enrich_min_confidence: 0.7,
            tmdb_api_token: None,
            anilist_enabled: false,
            metadata_language: None,
            oidc_issuer: None,
            oidc_client_id: None,
            oidc_client_secret: None,
            oidc_scopes: "openid profile email".to_string(),
            web_url: "http://localhost:5173".to_string(),
            extra_allowed_origins: None,
            admin_emails: None,
            cookie_secure: None,
            session_idle_days: 14,
            session_max_days: 60,
            rate_limit_enabled: true,
            rate_limit_auth_per_minute: 10,
            rate_limit_search_per_minute: 60,
            rate_limit_trust_forwarded_for: false,
        };
        let options = connect_options(&config);
        assert_eq!(options.get_max_connections(), Some(42));
        assert_eq!(options.get_min_connections(), Some(7));
        assert!(!options.get_sqlx_logging());
    }
}
