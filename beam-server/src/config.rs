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
}

impl ServerConfig {
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
