use confique::Config;
use std::fmt;
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

    #[error("Invalid value for {0}: {1}")]
    InvalidValue(String, String),
}

/// Application configuration
#[derive(Clone, Config)]
pub struct ServerConfig {
    #[config(env = "BEAM_BIND_ADDRESS", default = "0.0.0.0:8000")]
    pub bind_address: String,

    #[config(env = "BEAM_SERVER_URL", default = "http://localhost:8000")]
    pub server_url: String,

    #[config(env = "BEAM_ENABLE_METRICS", default = false)]
    pub enable_metrics: bool,

    /// How long a graceful shutdown (ctrl-c or SIGTERM, what container
    /// orchestrators send before a hard kill) waits for in-flight requests
    /// to drain before the process exits anyway, in seconds.
    #[config(env = "BEAM_SHUTDOWN_TIMEOUT_SECS", default = 30)]
    pub shutdown_timeout_secs: u64,

    #[config(env = "BEAM_VIDEO_DIR", default = "./videos")]
    pub video_dir: PathBuf,

    #[config(env = "BEAM_DATA_DIR", default = "./data")]
    pub data_dir: PathBuf,

    #[config(
        env = "BEAM_DATABASE_URL",
        default = "postgres://beam:password@localhost:5432/beam"
    )]
    pub database_url: String,

    /// Whether to apply pending database migrations at startup. On by
    /// default so a container-only deployment needs no separate migration
    /// step; disable for operator-managed migrations via the
    /// `beam-migration` CLI (`up`/`down`/`status`).
    #[config(env = "BEAM_AUTO_MIGRATE", default = true)]
    pub auto_migrate: bool,

    /// Maximum size of the Postgres connection pool.
    #[config(env = "BEAM_DB_MAX_CONNECTIONS", default = 20)]
    pub db_max_connections: u32,

    /// Connections the pool keeps open even when idle.
    #[config(env = "BEAM_DB_MIN_CONNECTIONS", default = 5)]
    pub db_min_connections: u32,

    /// Whether to hash files with unknown/unsupported extensions during
    /// indexing. Hashing them lets duplicate detection cover every file;
    /// disable to save scan IO.
    #[config(env = "BEAM_HASH_UNKNOWN_FILES", default = true)]
    pub hash_unknown_files: bool,

    /// Interval between periodic full rescans of every library, in seconds.
    /// Acts as the backstop that catches changes the filesystem watcher missed.
    #[config(env = "BEAM_SCAN_INTERVAL_SECS", default = 3600)]
    pub scan_interval_secs: u64,

    /// Whether to run the inotify-based filesystem watcher for near-real-time
    /// index updates. When false, only the startup scan and periodic rescans run.
    #[config(env = "BEAM_WATCH_ENABLED", default = true)]
    pub watch_enabled: bool,

    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    #[config(env = "BEAM_WATCH_DEBOUNCE_MS", default = 2000)]
    pub watch_debounce_ms: u64,

    /// Interval between metadata-enrichment sweeps, in seconds. New titles
    /// are also swept immediately when queued by a scan; this is the backstop
    /// for retries and anything the immediate poke missed.
    #[config(env = "BEAM_ENRICH_INTERVAL_SECS", default = 300)]
    pub enrich_interval_secs: u64,

    /// Maximum number of titles processed per metadata-enrichment sweep.
    /// Larger batches drain a backlog faster but make each sweep longer and
    /// lean harder on the provider's rate limits. Must be at least 1.
    #[config(env = "BEAM_ENRICH_BATCH_SIZE", default = 25)]
    pub enrich_batch_size: u32,

    /// Minimum overall match confidence, in `(0.0, 1.0]`, a provider candidate
    /// must reach before its metadata is applied to a title. Higher is
    /// stricter: fewer false matches, but more titles left un-enriched.
    #[config(env = "BEAM_ENRICH_MIN_CONFIDENCE", default = 0.7)]
    pub enrich_min_confidence: f64,

    /// TMDB API read-access token used by `cameo` for TMDB-sourced
    /// enrichment. If absent, TMDB-eligible titles are left un-enriched
    /// rather than failing the scan; AniList-sourced titles still enrich
    /// without it.
    #[config(env = "BEAM_TMDB_API_TOKEN")]
    pub tmdb_api_token: Option<String>,

    /// Toggles AniList-sourced enrichment via `cameo`.
    #[config(env = "BEAM_ANILIST_ENABLED", default = true)]
    pub anilist_enabled: bool,

    /// Preferred language for provider metadata, as a BCP-47 tag like `en`
    /// or `en-US` (lowercase language, uppercase region). Applied to the
    /// TMDB client only -- AniList has no language concept. Unset -> the
    /// provider's own default. An empty or whitespace-only value is
    /// normalized to unset (confique parses a set-but-empty env var as
    /// `Some("")`).
    #[config(env = "BEAM_METADATA_LANGUAGE")]
    pub metadata_language: Option<String>,

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

    /// Name of the ID-token claim the IdP asserts to grant admin (e.g.
    /// `groups`). Admin is derived solely from this claim, recomputed on every
    /// login (issue #85) -- there is no server-side email allowlist. Unset ->
    /// nobody is granted admin, and any existing admin is **demoted at their
    /// next login**. An empty value is treated as unset.
    #[config(env = "BEAM_OIDC_ADMIN_CLAIM")]
    pub oidc_admin_claim: Option<String>,

    /// Expected value of `oidc_admin_claim`. Unset -> the claim must assert
    /// boolean `true` (a stringified `"true"` is also accepted). Set -> admin
    /// is granted when the claim is a string equal to this value, or an array
    /// containing it (case-sensitive; covers a `groups` claim). Setting this
    /// while `BEAM_OIDC_ADMIN_CLAIM` is unset is a startup error. An empty
    /// value is treated as unset.
    #[config(env = "BEAM_OIDC_ADMIN_VALUE")]
    pub oidc_admin_value: Option<String>,

    /// Base URL of the web client; the OIDC callback redirects here on
    /// success, and it's implicitly allowed as a CSRF-safe request Origin.
    #[config(env = "BEAM_WEB_URL", default = "http://localhost:5173")]
    pub web_url: String,

    /// Comma-separated extra Origins to accept on state-changing requests,
    /// beyond `web_url` and the server's own origin (e.g. a second web
    /// client, a mobile app's custom scheme during dev).
    #[config(env = "BEAM_EXTRA_ALLOWED_ORIGINS")]
    pub extra_allowed_origins: Option<String>,

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

    /// Whether the in-process rate limiter is installed on the auth and search
    /// endpoints (see `routes::rate_limit`, NFR-107). When false, no limiter
    /// hoops are mounted at all.
    #[config(env = "BEAM_RATE_LIMIT_ENABLED", default = true)]
    pub rate_limit_enabled: bool,

    /// Sustained request rate — and burst capacity — for the auth endpoints
    /// (`/v1/auth/login`, `/v1/auth/callback`), per client key, in requests per
    /// minute. Must be at least 1.
    #[config(env = "BEAM_RATE_LIMIT_AUTH_PER_MINUTE", default = 10)]
    pub rate_limit_auth_per_minute: u32,

    /// Sustained request rate — and burst capacity — for the media
    /// browse/search endpoint (`GET /v1/media`), per client key, in requests
    /// per minute. Must be at least 1.
    #[config(env = "BEAM_RATE_LIMIT_SEARCH_PER_MINUTE", default = 60)]
    pub rate_limit_search_per_minute: u32,

    /// Whether to trust a client-supplied `X-Forwarded-For` header when
    /// deriving the rate-limit client key. Off by default: the header is
    /// trivially spoofable unless the server sits behind a trusted proxy that
    /// overwrites it. When off, the peer socket IP is used.
    #[config(env = "BEAM_RATE_LIMIT_TRUST_FORWARDED_FOR", default = false)]
    pub rate_limit_trust_forwarded_for: bool,
}

/// Hand-written so the startup "Configuration loaded" log line can never
/// leak credentials: secrets are redacted here rather than at each log site.
/// All fields are destructured (no `..`) so adding a config field without
/// deciding whether it is a secret is a compile error.
impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            bind_address,
            server_url,
            enable_metrics,
            shutdown_timeout_secs,
            video_dir,
            data_dir,
            database_url,
            auto_migrate,
            db_max_connections,
            db_min_connections,
            hash_unknown_files,
            scan_interval_secs,
            watch_enabled,
            watch_debounce_ms,
            enrich_interval_secs,
            enrich_batch_size,
            enrich_min_confidence,
            tmdb_api_token,
            anilist_enabled,
            metadata_language,
            oidc_issuer,
            oidc_client_id,
            oidc_client_secret,
            oidc_scopes,
            oidc_admin_claim,
            oidc_admin_value,
            web_url,
            extra_allowed_origins,
            cookie_secure,
            session_idle_days,
            session_max_days,
            rate_limit_enabled,
            rate_limit_auth_per_minute,
            rate_limit_search_per_minute,
            rate_limit_trust_forwarded_for,
        } = self;
        f.debug_struct("ServerConfig")
            .field("bind_address", bind_address)
            .field("server_url", server_url)
            .field("enable_metrics", enable_metrics)
            .field("shutdown_timeout_secs", shutdown_timeout_secs)
            .field("video_dir", video_dir)
            .field("data_dir", data_dir)
            .field("database_url", &redact_url_password(database_url))
            .field("auto_migrate", auto_migrate)
            .field("db_max_connections", db_max_connections)
            .field("db_min_connections", db_min_connections)
            .field("hash_unknown_files", hash_unknown_files)
            .field("scan_interval_secs", scan_interval_secs)
            .field("watch_enabled", watch_enabled)
            .field("watch_debounce_ms", watch_debounce_ms)
            .field("enrich_interval_secs", enrich_interval_secs)
            .field("enrich_batch_size", enrich_batch_size)
            .field("enrich_min_confidence", enrich_min_confidence)
            .field("tmdb_api_token", &redact_option(tmdb_api_token))
            .field("anilist_enabled", anilist_enabled)
            .field("metadata_language", metadata_language)
            .field("oidc_issuer", oidc_issuer)
            .field("oidc_client_id", oidc_client_id)
            .field("oidc_client_secret", &redact_option(oidc_client_secret))
            .field("oidc_scopes", oidc_scopes)
            .field("oidc_admin_claim", oidc_admin_claim)
            .field("oidc_admin_value", oidc_admin_value)
            .field("web_url", web_url)
            .field("extra_allowed_origins", extra_allowed_origins)
            .field("cookie_secure", cookie_secure)
            .field("session_idle_days", session_idle_days)
            .field("session_max_days", session_max_days)
            .field("rate_limit_enabled", rate_limit_enabled)
            .field("rate_limit_auth_per_minute", rate_limit_auth_per_minute)
            .field("rate_limit_search_per_minute", rate_limit_search_per_minute)
            .field(
                "rate_limit_trust_forwarded_for",
                rate_limit_trust_forwarded_for,
            )
            .finish()
    }
}

/// Renders an `Option` secret without its value.
fn redact_option(secret: &Option<String>) -> Option<&'static str> {
    secret.as_ref().map(|_| "<redacted>")
}

/// Replaces the password component of a `scheme://user:password@host/...`
/// URL with `<redacted>`. URLs without a `user:password@` userinfo section
/// are returned unchanged.
fn redact_url_password(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return url.to_string();
    };
    let userinfo = &rest[..at];
    let Some(colon) = userinfo.find(':') else {
        return url.to_string();
    };
    format!(
        "{}{}:<redacted>{}",
        &url[..scheme_end + 3],
        &userinfo[..colon],
        &rest[at..]
    )
}

/// Startup assessment of the cookie `Secure`-flag configuration; see
/// [`ServerConfig::cookie_security_verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSecurityVerdict {
    /// Cookies are Secure, or nothing about the configuration implies an
    /// HTTPS deployment.
    Ok,
    /// The operator explicitly set `BEAM_COOKIE_SECURE=false` while other
    /// origins imply HTTPS -- honored (e.g. TLS terminated in front of a
    /// plain-HTTP origin during debugging), but worth a loud warning.
    WarnExplicitInsecure,
    /// Cookies resolved insecure purely from `server_url`'s scheme while
    /// `web_url`/`extra_allowed_origins` imply a real HTTPS deployment and
    /// no explicit override was given. Almost certainly a misconfiguration
    /// (the session cookie would ship without `Secure` on a production
    /// HTTPS site), so startup refuses to continue.
    ErrLikelyMisconfigured,
}

impl ServerConfig {
    /// `database_url` with any password redacted, safe for logs.
    pub fn redacted_database_url(&self) -> String {
        redact_url_password(&self.database_url)
    }

    /// Classifies the cookie-`Secure` configuration. `cookie_secure`
    /// defaults to `server_url`'s scheme, but behind a TLS-terminating
    /// reverse proxy (e.g. the Traefik topology in compose.beam.yaml) the
    /// externally-visible scheme can differ -- if any other configured
    /// origin looks like HTTPS while cookies resolve insecure, that's a
    /// misconfiguration unless the operator explicitly opted out.
    pub fn cookie_security_verdict(&self) -> CookieSecurityVerdict {
        let https_implied = self.web_url.starts_with("https://")
            || self
                .extra_allowed_origins
                .as_deref()
                .is_some_and(|origins| origins.contains("https://"));

        if self.resolved_cookie_secure() || !https_implied {
            CookieSecurityVerdict::Ok
        } else if self.cookie_secure == Some(false) {
            CookieSecurityVerdict::WarnExplicitInsecure
        } else {
            CookieSecurityVerdict::ErrLikelyMisconfigured
        }
    }

    /// Resolves `cookie_secure`, defaulting to whether `server_url` is
    /// `https://...` when not explicitly overridden.
    pub fn resolved_cookie_secure(&self) -> bool {
        self.cookie_secure
            .unwrap_or_else(|| self.server_url.starts_with("https://"))
    }

    /// The OIDC redirect URL registered with the IdP: always this server's
    /// own callback endpoint, never the web client's origin.
    pub fn oidc_redirect_url(&self) -> String {
        format!("{}/v1/auth/callback", self.server_url)
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
        let mut config = Self::builder().env().load()?;

        // 2. Normalize before validating: a set-but-empty optional env var
        // arrives as `Some("")` (confique parses presence, not content) and
        // must mean the same thing as unset.
        config.normalize_values();

        // 3. Validate scalar values (ranges, non-empty) before touching the
        // filesystem: a bad number should fail fast without side effects.
        config.validate_values()?;

        // 4. Validate paths and ensure writeable directories exist
        config.validate_paths()?;

        Ok(config)
    }

    /// Canonicalizes values whose raw env form is ambiguous: a value that is
    /// empty (or whitespace-only) after trimming becomes `None`, so an operator
    /// commenting a var out and setting it to `""` are equivalent. Confique
    /// parses a set-but-empty optional env var as `Some("")`, which this
    /// undoes.
    fn normalize_values(&mut self) {
        fn empty_to_none(value: Option<String>) -> Option<String> {
            value
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }

        self.metadata_language = empty_to_none(self.metadata_language.take());
        self.oidc_admin_claim = empty_to_none(self.oidc_admin_claim.take());
        self.oidc_admin_value = empty_to_none(self.oidc_admin_value.take());
    }

    /// Validates scalar configuration values that confique's type-level
    /// parsing can't (ranges, mutually-consistent bounds). Pure and
    /// side-effect-free so it can be unit-tested against a constructed config.
    fn validate_values(&self) -> Result<(), ConfigError> {
        if self.enrich_batch_size == 0 {
            return Err(ConfigError::InvalidValue(
                "BEAM_ENRICH_BATCH_SIZE".to_string(),
                "must be at least 1".to_string(),
            ));
        }

        if !(self.enrich_min_confidence > 0.0 && self.enrich_min_confidence <= 1.0) {
            return Err(ConfigError::InvalidValue(
                "BEAM_ENRICH_MIN_CONFIDENCE".to_string(),
                format!(
                    "must be in the range (0.0, 1.0], got {}",
                    self.enrich_min_confidence
                ),
            ));
        }

        if self.rate_limit_auth_per_minute == 0 {
            return Err(ConfigError::InvalidValue(
                "BEAM_RATE_LIMIT_AUTH_PER_MINUTE".to_string(),
                "must be at least 1".to_string(),
            ));
        }

        if self.rate_limit_search_per_minute == 0 {
            return Err(ConfigError::InvalidValue(
                "BEAM_RATE_LIMIT_SEARCH_PER_MINUTE".to_string(),
                "must be at least 1".to_string(),
            ));
        }

        // An expected admin-claim value is meaningless without the claim to
        // read it from -- reject the half-configured combination up front
        // rather than silently granting admin to nobody.
        if self.oidc_admin_value.is_some() && self.oidc_admin_claim.is_none() {
            return Err(ConfigError::InvalidValue(
                "BEAM_OIDC_ADMIN_VALUE".to_string(),
                "requires BEAM_OIDC_ADMIN_CLAIM to also be set".to_string(),
            ));
        }

        Ok(())
    }

    /// Validates configuration paths
    fn validate_paths(&self) -> Result<(), ConfigError> {
        // VIDEO_DIR must exist (read-only mount)
        if !self.video_dir.exists() {
            return Err(ConfigError::DirNotFoundError(
                self.video_dir.display().to_string(),
            ));
        }

        // BEAM_DATA_DIR can be created, so just ensure parent exists
        if let Some(parent) = self.data_dir.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                ConfigError::DirCreationError(
                    "BEAM_DATA_DIR".to_string(),
                    self.data_dir.display().to_string(),
                    e,
                )
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully-populated config with known secret values, for asserting that
    /// none of them survive into `Debug` output.
    fn config_with_secrets() -> ServerConfig {
        ServerConfig {
            bind_address: "0.0.0.0:8000".to_string(),
            server_url: "https://beam.example.com".to_string(),
            enable_metrics: false,
            shutdown_timeout_secs: 30,
            video_dir: PathBuf::from("/videos"),
            data_dir: PathBuf::from("/cache"),
            database_url: "postgres://beam:db-secret-pw@localhost:5432/beam".to_string(),
            auto_migrate: true,
            db_max_connections: 20,
            db_min_connections: 5,
            hash_unknown_files: true,
            scan_interval_secs: 3600,
            watch_enabled: true,
            watch_debounce_ms: 2000,
            enrich_interval_secs: 300,
            enrich_batch_size: 25,
            enrich_min_confidence: 0.7,
            tmdb_api_token: Some("tmdb-secret-token".to_string()),
            anilist_enabled: true,
            metadata_language: None,
            oidc_issuer: Some("https://idp.example.com".to_string()),
            oidc_client_id: Some("beam-client".to_string()),
            oidc_client_secret: Some("oidc-secret-value".to_string()),
            oidc_scopes: "openid profile email".to_string(),
            oidc_admin_claim: Some("groups".to_string()),
            oidc_admin_value: Some("beam-admin".to_string()),
            web_url: "https://beam.example.com".to_string(),
            extra_allowed_origins: None,
            cookie_secure: None,
            session_idle_days: 14,
            session_max_days: 60,
            rate_limit_enabled: true,
            rate_limit_auth_per_minute: 10,
            rate_limit_search_per_minute: 60,
            rate_limit_trust_forwarded_for: false,
        }
    }

    #[test]
    fn debug_output_redacts_every_secret() {
        let config = config_with_secrets();
        let output = format!("{config:?}");

        assert!(!output.contains("db-secret-pw"), "output: {output}");
        assert!(!output.contains("tmdb-secret-token"), "output: {output}");
        assert!(!output.contains("oidc-secret-value"), "output: {output}");
        assert!(output.contains("<redacted>"), "output: {output}");
        // Non-secret fields stay visible for operator debugging.
        assert!(output.contains("beam.example.com"), "output: {output}");
        assert!(output.contains("beam-client"), "output: {output}");
    }

    #[test]
    fn redacted_database_url_hides_password_only() {
        let config = config_with_secrets();
        assert_eq!(
            config.redacted_database_url(),
            "postgres://beam:<redacted>@localhost:5432/beam"
        );
    }

    #[test]
    fn cookie_security_verdict_covers_scheme_and_override_combinations() {
        // (server_url, web_url, extra_origins, explicit override, expected)
        let cases = [
            // Plain-HTTP local dev: nothing implies HTTPS.
            (
                "http://localhost:8000",
                "http://localhost:5173",
                None,
                None,
                CookieSecurityVerdict::Ok,
            ),
            // Fully-HTTPS deployment: heuristic resolves Secure.
            (
                "https://beam.example.com",
                "https://beam.example.com",
                None,
                None,
                CookieSecurityVerdict::Ok,
            ),
            // TLS-terminating proxy in front of a plain-HTTP origin, no
            // override: the classic footgun -- refuse to start.
            (
                "http://localhost:8000",
                "https://beam.example.com",
                None,
                None,
                CookieSecurityVerdict::ErrLikelyMisconfigured,
            ),
            // Same, implied only via an extra allowed origin.
            (
                "http://localhost:8000",
                "http://localhost:5173",
                Some("https://other.example.com"),
                None,
                CookieSecurityVerdict::ErrLikelyMisconfigured,
            ),
            // Proxy topology fixed by an explicit opt-in to Secure cookies.
            (
                "http://localhost:8000",
                "https://beam.example.com",
                None,
                Some(true),
                CookieSecurityVerdict::Ok,
            ),
            // Explicit opt-out is honored but flagged.
            (
                "http://localhost:8000",
                "https://beam.example.com",
                None,
                Some(false),
                CookieSecurityVerdict::WarnExplicitInsecure,
            ),
            // Explicit opt-out with nothing HTTPS-like at all: plain Ok.
            (
                "http://localhost:8000",
                "http://localhost:5173",
                None,
                Some(false),
                CookieSecurityVerdict::Ok,
            ),
        ];

        for (server_url, web_url, extra, cookie_secure, expected) in cases {
            let config = ServerConfig {
                server_url: server_url.to_string(),
                web_url: web_url.to_string(),
                extra_allowed_origins: extra.map(str::to_string),
                cookie_secure,
                ..config_with_secrets()
            };
            assert_eq!(
                config.cookie_security_verdict(),
                expected,
                "server_url={server_url}, web_url={web_url}, extra={extra:?}, \
                 override={cookie_secure:?}"
            );
        }
    }

    #[test]
    fn auto_migrate_defaults_to_enabled() {
        // Container-only deployments rely on this default for schema setup;
        // see docs/operations/deployment.md.
        let config = ServerConfig::builder()
            .load()
            .expect("defaults-only config should load");
        assert!(config.auto_migrate);
    }

    #[test]
    fn enrichment_defaults_pass_validation() {
        // The compiled-in defaults must themselves be valid, or a
        // defaults-only deployment would refuse to start.
        let config = ServerConfig {
            enrich_batch_size: 25,
            enrich_min_confidence: 0.7,
            ..config_with_secrets()
        };
        assert!(config.validate_values().is_ok());
    }

    #[test]
    fn zero_enrich_batch_size_is_rejected() {
        let config = ServerConfig {
            enrich_batch_size: 0,
            ..config_with_secrets()
        };
        let err = config
            .validate_values()
            .expect_err("batch size 0 must be rejected");
        assert!(
            matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_BATCH_SIZE"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn out_of_range_enrich_min_confidence_is_rejected() {
        // (0.0, 1.0]: the open lower bound rejects 0.0, the closed upper bound
        // accepts exactly 1.0 but rejects anything above it.
        for bad in [0.0_f64, -0.1, 1.5] {
            let config = ServerConfig {
                enrich_min_confidence: bad,
                ..config_with_secrets()
            };
            let err = config
                .validate_values()
                .expect_err("min confidence outside (0.0, 1.0] must be rejected");
            assert!(
                matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_MIN_CONFIDENCE"),
                "value {bad} produced unexpected error: {err}"
            );
        }

        // Boundary that must be accepted.
        let config = ServerConfig {
            enrich_min_confidence: 1.0,
            ..config_with_secrets()
        };
        assert!(
            config.validate_values().is_ok(),
            "1.0 is a valid upper bound"
        );
    }

    #[test]
    fn zero_rate_limit_per_minute_is_rejected() {
        for field in [
            "BEAM_RATE_LIMIT_AUTH_PER_MINUTE",
            "BEAM_RATE_LIMIT_SEARCH_PER_MINUTE",
        ] {
            let config = ServerConfig {
                rate_limit_auth_per_minute: if field.contains("AUTH") { 0 } else { 10 },
                rate_limit_search_per_minute: if field.contains("SEARCH") { 0 } else { 60 },
                ..config_with_secrets()
            };
            let err = config
                .validate_values()
                .expect_err("a zero per-minute rate must be rejected");
            assert!(
                matches!(&err, ConfigError::InvalidValue(f, _) if f == field),
                "unexpected error for {field}: {err}"
            );
        }
    }

    #[test]
    fn admin_value_without_admin_claim_is_rejected() {
        let config = ServerConfig {
            oidc_admin_claim: None,
            oidc_admin_value: Some("beam-admin".to_string()),
            ..config_with_secrets()
        };
        let err = config
            .validate_values()
            .expect_err("BEAM_OIDC_ADMIN_VALUE without BEAM_OIDC_ADMIN_CLAIM must be rejected");
        assert!(
            matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_OIDC_ADMIN_VALUE"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn admin_claim_configurations_pass_validation() {
        // Both unset (nobody admin), claim-only (boolean claim), and
        // claim+value (string/array claim) are all valid.
        for (claim, value) in [
            (None, None),
            (Some("is_admin"), None),
            (Some("groups"), Some("beam-admin")),
        ] {
            let config = ServerConfig {
                oidc_admin_claim: claim.map(str::to_string),
                oidc_admin_value: value.map(str::to_string),
                ..config_with_secrets()
            };
            assert!(
                config.validate_values().is_ok(),
                "claim={claim:?} value={value:?} should validate"
            );
        }
    }

    #[test]
    fn admin_claim_and_value_normalization_maps_empty_to_none() {
        // Set-but-empty env vars (confique yields Some("")) must mean unset,
        // so a half-empty pair doesn't later trip the validation error.
        let mut config = ServerConfig {
            oidc_admin_claim: Some("  ".to_string()),
            oidc_admin_value: Some("".to_string()),
            ..config_with_secrets()
        };
        config.normalize_values();
        assert_eq!(config.oidc_admin_claim, None);
        assert_eq!(config.oidc_admin_value, None);

        // A real value is trimmed but preserved.
        let mut config = ServerConfig {
            oidc_admin_claim: Some(" groups ".to_string()),
            oidc_admin_value: Some(" beam-admin ".to_string()),
            ..config_with_secrets()
        };
        config.normalize_values();
        assert_eq!(config.oidc_admin_claim.as_deref(), Some("groups"));
        assert_eq!(config.oidc_admin_value.as_deref(), Some("beam-admin"));
    }

    #[test]
    fn metadata_language_normalization_maps_empty_and_whitespace_to_none() {
        // (raw value as loaded, expected after normalization)
        let cases: [(Option<&str>, Option<&str>); 5] = [
            // Set-but-empty env var: confique yields Some("") -- must mean unset.
            (Some(""), None),
            (Some("   "), None),
            // Surrounding whitespace is trimmed off a real value.
            (Some("  en-US  "), Some("en-US")),
            // A clean value survives untouched.
            (Some("ja"), Some("ja")),
            (None, None),
        ];

        for (raw, expected) in cases {
            let mut config = ServerConfig {
                metadata_language: raw.map(str::to_string),
                ..config_with_secrets()
            };
            config.normalize_values();
            assert_eq!(
                config.metadata_language.as_deref(),
                expected,
                "raw value {raw:?} normalized incorrectly"
            );
        }
    }

    #[test]
    fn redact_url_password_handles_urls_without_credentials() {
        // No userinfo at all.
        assert_eq!(
            redact_url_password("postgres://localhost:5432/beam"),
            "postgres://localhost:5432/beam"
        );
        // User without password.
        assert_eq!(
            redact_url_password("postgres://beam@localhost/beam"),
            "postgres://beam@localhost/beam"
        );
        // Not a URL: returned unchanged rather than panicking.
        assert_eq!(redact_url_password("not a url"), "not a url");
        // An `@` in the path must not be mistaken for userinfo.
        assert_eq!(
            redact_url_password("postgres://localhost/db@name"),
            "postgres://localhost/db@name"
        );
    }
}
