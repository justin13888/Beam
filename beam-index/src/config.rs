use confique::Config;

#[derive(Debug, Clone, Config)]
pub struct IndexConfig {
    #[config(
        env = "DATABASE_URL",
        default = "postgres://beam:password@localhost:5432/beam"
    )]
    pub database_url: String,

    #[config(env = "BIND_HOST", default = "0.0.0.0")]
    pub host: String,

    #[config(env = "GRPC_PORT", default = 50051)]
    pub port: u16,

    /// Interval between periodic full rescans of every library, in seconds.
    /// Acts as the backstop that catches changes the watcher missed.
    #[config(env = "SCAN_INTERVAL_SECS", default = 3600)]
    pub scan_interval_secs: u64,

    /// Whether to run the inotify-based filesystem watcher. When false, only
    /// the startup scan and the periodic rescans run.
    #[config(env = "WATCH_ENABLED", default = true)]
    pub watch_enabled: bool,

    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    #[config(env = "WATCH_DEBOUNCE_MS", default = 2000)]
    pub watch_debounce_ms: u64,

    /// Whether to hash files with unknown/unsupported extensions. Hashing them
    /// lets duplicate detection cover every file; disable to save scan IO.
    #[config(env = "HASH_UNKNOWN_FILES", default = true)]
    pub hash_unknown_files: bool,
}
