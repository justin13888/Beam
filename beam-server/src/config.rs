use confique::Config;
use std::path::PathBuf;
use thiserror::Error;

/// Configuration error type
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Configuration loading error: {0}")]
    LoadError(#[from] confique::Error),

    #[error("Failed to create directory '{1}' for '{0}': {2}")]
    DirCreationError(String, String, std::io::Error),

    #[error("Directory not found: {0}")]
    DirNotFoundError(String),
}

/// Application configuration
#[derive(Debug, Clone, Config)]
pub struct ServerConfig {
    #[config(env = "BIND_ADDRESS", default = "0.0.0.0:8000")]
    pub bind_address: String,

    #[config(env = "SERVER_URL", default = "http://localhost:8000")]
    pub server_url: String,

    #[config(env = "ENABLE_METRICS", default = false)]
    pub enable_metrics: bool,

    #[config(env = "VIDEO_DIR", default = "./videos")]
    pub video_dir: PathBuf,

    #[config(env = "CACHE_DIR", default = "./cache")]
    pub cache_dir: PathBuf,

    #[config(
        env = "DATABASE_URL",
        default = "postgres://beam:password@localhost:5432/beam"
    )]
    pub database_url: String,

    #[config(env = "JWT_SECRET")]
    pub jwt_secret: String,

    /// Whether to hash files with unknown/unsupported extensions during
    /// indexing. Hashing them lets duplicate detection cover every file;
    /// disable to save scan IO.
    #[config(env = "HASH_UNKNOWN_FILES", default = true)]
    pub hash_unknown_files: bool,

    /// Interval between periodic full rescans of every library, in seconds.
    /// Acts as the backstop that catches changes the filesystem watcher missed.
    #[config(env = "SCAN_INTERVAL_SECS", default = 3600)]
    pub scan_interval_secs: u64,

    /// Whether to run the inotify-based filesystem watcher for near-real-time
    /// index updates. When false, only the startup scan and periodic rescans run.
    #[config(env = "WATCH_ENABLED", default = true)]
    pub watch_enabled: bool,

    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    #[config(env = "WATCH_DEBOUNCE_MS", default = 2000)]
    pub watch_debounce_ms: u64,

    /// Interval between metadata-enrichment sweeps, in seconds. New titles
    /// are also swept immediately when queued by a scan; this is the backstop
    /// for retries and anything the immediate poke missed.
    #[config(env = "ENRICH_INTERVAL_SECS", default = 300)]
    pub enrich_interval_secs: u64,

    /// TMDB API read-access token used by `cameo` for TMDB-sourced
    /// enrichment. If absent, TMDB-eligible titles are left un-enriched
    /// rather than failing the scan; AniList-sourced titles still enrich
    /// without it.
    #[config(env = "TMDB_API_TOKEN")]
    pub tmdb_api_token: Option<String>,

    /// Toggles AniList-sourced enrichment via `cameo`.
    #[config(env = "ANILIST_ENABLED", default = true)]
    pub anilist_enabled: bool,

    /// OIDC issuer URL (e.g. Dex in dev: `http://localhost:5556/dex`). OIDC
    /// login is disabled -- returning a clear error rather than panicking --
    /// unless this, `OIDC_CLIENT_ID`, and `OIDC_CLIENT_SECRET` are all set
    /// (see ADR-0003).
    #[config(env = "BEAM_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,

    #[config(env = "BEAM_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,

    #[config(env = "BEAM_OIDC_CLIENT_SECRET")]
    pub oidc_client_secret: Option<String>,

    /// Space-separated OIDC scopes requested at login.
    #[config(env = "BEAM_OIDC_SCOPES", default = "openid profile email")]
    pub oidc_scopes: String,

    /// Base URL of the web client; the OIDC callback redirects here on
    /// success, and it's implicitly allowed as a CSRF-safe request Origin.
    #[config(env = "BEAM_WEB_URL", default = "http://localhost:5173")]
    pub web_url: String,

    /// Comma-separated extra Origins to accept on state-changing requests,
    /// beyond `web_url` and the server's own origin (e.g. a second web
    /// client, a mobile app's custom scheme during dev).
    #[config(env = "BEAM_EXTRA_ALLOWED_ORIGINS")]
    pub extra_allowed_origins: Option<String>,

    /// Comma-separated, case-insensitive allowlist of emails granted admin
    /// on OIDC login. An unverified email is never granted admin regardless
    /// of allowlist membership.
    #[config(env = "BEAM_ADMIN_EMAILS")]
    pub admin_emails: Option<String>,

    /// Whether auth cookies are marked `Secure`. Defaults to whatever
    /// `server_url`'s scheme implies (`https` -> secure) when unset; only
    /// override for a topology where that heuristic is wrong (e.g. TLS
    /// terminated in front of a plain-HTTP origin).
    #[config(env = "BEAM_COOKIE_SECURE")]
    pub cookie_secure: Option<bool>,

    /// Session idle timeout, in days: how long a session survives with no
    /// activity before it must re-authenticate. Slides forward on activity,
    /// capped by `session_max_days`.
    #[config(env = "BEAM_SESSION_IDLE_DAYS", default = 14)]
    pub session_idle_days: u64,

    /// Absolute session lifetime, in days, regardless of activity.
    #[config(env = "BEAM_SESSION_MAX_DAYS", default = 60)]
    pub session_max_days: u64,
}

impl ServerConfig {
    /// Resolves `cookie_secure`, defaulting to whether `server_url` is
    /// `https://...` when not explicitly overridden.
    pub fn resolved_cookie_secure(&self) -> bool {
        self.cookie_secure
            .unwrap_or_else(|| self.server_url.starts_with("https://"))
    }

    /// The OIDC redirect URL registered with the IdP: always this server's
    /// own callback endpoint, never the web client's origin.
    pub fn oidc_redirect_url(&self) -> String {
        format!("{}/v1/auth/oidc/callback", self.server_url)
    }

    /// Whether enough OIDC configuration is present to attempt discovery.
    /// All three of issuer/client_id/client_secret are required together.
    pub fn oidc_configured(&self) -> bool {
        self.oidc_issuer.is_some()
            && self.oidc_client_id.is_some()
            && self.oidc_client_secret.is_some()
    }

    /// Load configuration from environment variables and validate paths
    pub fn load_and_validate() -> Result<Self, ConfigError> {
        // 1. Load the configuration purely from environment variables
        let config = Self::builder().env().load()?;

        // 2. Validate paths and ensure writeable directories exist
        config.validate_paths()?;

        Ok(config)
    }

    /// Validates configuration paths
    fn validate_paths(&self) -> Result<(), ConfigError> {
        // VIDEO_DIR must exist (read-only mount)
        if !self.video_dir.exists() {
            return Err(ConfigError::DirNotFoundError(
                self.video_dir.display().to_string(),
            ));
        }

        // CACHE_DIR can be created, so just ensure parent exists
        if let Some(parent) = self.cache_dir.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                ConfigError::DirCreationError(
                    "CACHE_DIR".to_string(),
                    self.cache_dir.display().to_string(),
                    e,
                )
            })?;
        }

        Ok(())
    }
}
