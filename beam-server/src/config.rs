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

/// A configuration with every declared default applied and every optional
/// field unset -- what the process would load with no environment and no
/// config file.
///
/// Derived from the `#[config(default = ...)]` attributes via confique's
/// generated layer type rather than restated field by field, so it cannot
/// drift from the declarations and adding a field does not break any caller.
/// Tests build the config they need as `ServerConfig { video_dir: ...,
/// ..Default::default() }` instead of writing out all forty fields.
impl Default for ServerConfig {
    fn default() -> Self {
        Self::from_layer(<<Self as Config>::Layer as confique::Layer>::default_values())
            .expect("every non-optional field of ServerConfig declares a default")
    }
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

    /// OIDC issuer URL (e.g. the opt-in dev Dex: `http://dex.beam.localhost:5556/dex`). OIDC
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

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

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

    /// How long a session survives without activity, in seconds.
    ///
    /// Named because the `days * 24 * 60 * 60` conversion appeared inline at
    /// the call sites, where a mistyped operator yields a session that expires
    /// in minutes -- non-zero, apparently working, and impossible to notice
    /// without reading the arithmetic.
    pub fn session_idle_ttl_secs(&self) -> u64 {
        self.session_idle_days * SECONDS_PER_DAY
    }

    /// Hard ceiling on session lifetime from creation, in seconds.
    pub fn session_absolute_ttl_secs(&self) -> u64 {
        self.session_max_days * SECONDS_PER_DAY
    }

    /// Whether enough OIDC configuration is present to attempt discovery.
    /// All three of issuer/client_id/client_secret are required together.
    pub fn oidc_configured(&self) -> bool {
        self.oidc_issuer.is_some()
            && self.oidc_client_id.is_some()
            && self.oidc_client_secret.is_some()
    }

    /// Load configuration from environment variables and validate it.
    ///
    /// `#[mutants::skip]`: the body is two lines, and the half that is not
    /// already covered is `Self::builder().env().load()`, which reads the
    /// process environment. Exercising it means mutating that environment,
    /// which is `unsafe` in Rust 2024 precisely because the suite runs in
    /// parallel. Everything decidable has been moved into
    /// [`Self::normalize_and_validate`], which is tested directly. See
    /// ADR-0011's decision log.
    #[mutants::skip]
    pub fn load_and_validate() -> Result<Self, ConfigError> {
        Self::builder().env().load()?.normalize_and_validate()
    }

    /// Canonicalize and check an already-loaded configuration.
    ///
    /// Separated from [`Self::load_and_validate`] so the ordering is testable
    /// without touching the process environment. The order matters: normalize
    /// first, because a set-but-empty optional env var arrives as `Some("")`
    /// (confique parses presence, not content) and must mean the same thing as
    /// unset; then scalar validation, so a bad number fails fast without
    /// touching the filesystem; then paths, which create directories.
    pub(crate) fn normalize_and_validate(mut self) -> Result<Self, ConfigError> {
        self.normalize_values();
        self.validate_values()?;
        self.validate_paths()?;
        Ok(self)
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
        // VIDEO_DIR must exist and be a directory (read-only mount). `exists()`
        // alone would accept a regular file, and the scanner would then walk
        // nothing and report an empty library rather than a misconfiguration.
        if !self.video_dir.is_dir() {
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
            server_url: "https://beam.example.com".to_string(),
            video_dir: PathBuf::from("/videos"),
            data_dir: PathBuf::from("/cache"),
            database_url: "postgres://beam:db-secret-pw@localhost:5432/beam".to_string(),
            tmdb_api_token: Some("tmdb-secret-token".to_string()),
            oidc_issuer: Some("https://idp.example.com".to_string()),
            oidc_client_id: Some("beam-client".to_string()),
            oidc_client_secret: Some("oidc-secret-value".to_string()),
            oidc_admin_claim: Some("groups".to_string()),
            oidc_admin_value: Some("beam-admin".to_string()),
            web_url: "https://beam.example.com".to_string(),
            ..Default::default()
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
    fn a_set_but_empty_optional_setting_means_the_same_as_unset() {
        // `BEAM_METADATA_LANGUAGE=` in a compose file arrives as `Some("")`.
        // Left that way it reaches cameo as a language tag and fails the
        // client build -- which, being explicit, refuses to start the server.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("videos")).unwrap();

        let validated = ServerConfig {
            video_dir: temp.path().join("videos"),
            data_dir: temp.path().join("data"),
            metadata_language: Some("   ".to_string()),
            oidc_admin_claim: Some(String::new()),
            oidc_admin_value: None,
            ..Default::default()
        }
        .normalize_and_validate()
        .expect("blank optional settings are not a misconfiguration");

        assert_eq!(validated.metadata_language, None);
        assert_eq!(validated.oidc_admin_claim, None);
    }

    #[test]
    fn a_value_is_normalised_before_it_is_validated() {
        // Order matters: `BEAM_OIDC_ADMIN_VALUE` set with `BEAM_OIDC_ADMIN_CLAIM`
        // blank is the half-configured combination `validate_values` rejects --
        // but only once the blank claim has been normalised to `None`.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("videos")).unwrap();

        let error = ServerConfig {
            video_dir: temp.path().join("videos"),
            data_dir: temp.path().join("data"),
            oidc_admin_claim: Some("  ".to_string()),
            oidc_admin_value: Some("beam-admin".to_string()),
            ..Default::default()
        }
        .normalize_and_validate()
        .expect_err("a blank claim with a value set is still half-configured");

        assert!(
            matches!(&error, ConfigError::InvalidValue(field, _) if field == "BEAM_OIDC_ADMIN_VALUE"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_bad_scalar_fails_before_any_directory_is_created() {
        // `validate_values` runs before `validate_paths` so a typo'd number
        // does not leave a half-made data directory behind.
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("state").join("beam");

        let error = ServerConfig {
            video_dir: temp.path().join("videos"),
            data_dir: data_dir.clone(),
            enrich_batch_size: 0,
            ..Default::default()
        }
        .normalize_and_validate()
        .expect_err("a zero batch size is rejected");

        assert!(
            matches!(&error, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_BATCH_SIZE"),
            "unexpected error: {error}"
        );
        assert!(
            !temp.path().join("state").exists(),
            "no filesystem side effect before the values are known to be valid"
        );
    }

    #[test]
    fn the_oidc_redirect_url_points_at_this_servers_own_callback() {
        // It is sent to the identity provider as the place to come back to and
        // is matched exactly against the client's registered redirect URIs, so
        // it must be the *server* origin -- not the web origin, and not empty.
        let config = ServerConfig {
            server_url: "https://beam.example.com".to_string(),
            web_url: "https://app.example.com".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config.oidc_redirect_url(),
            "https://beam.example.com/v1/auth/callback"
        );
    }

    #[test]
    fn session_days_are_converted_to_seconds() {
        // 14 days is 1_209_600 seconds. A mistyped conversion still produces a
        // plausible-looking number and signs everyone out within minutes.
        let config = ServerConfig {
            session_idle_days: 14,
            session_max_days: 60,
            ..Default::default()
        };
        assert_eq!(config.session_idle_ttl_secs(), 1_209_600);
        assert_eq!(config.session_absolute_ttl_secs(), 5_184_000);
        assert_eq!(
            ServerConfig {
                session_idle_days: 1,
                ..Default::default()
            }
            .session_idle_ttl_secs(),
            86_400
        );
    }

    #[test]
    fn oidc_is_configured_only_when_all_three_settings_are_present() {
        // Any one of them missing means discovery cannot succeed; treating a
        // partial configuration as complete turns a clear "not configured"
        // startup message into a discovery failure at first login.
        let all = |issuer, client_id, secret| ServerConfig {
            oidc_issuer: issuer,
            oidc_client_id: client_id,
            oidc_client_secret: secret,
            ..Default::default()
        };
        let set = || Some("x".to_string());

        assert!(all(set(), set(), set()).oidc_configured());
        assert!(!all(None, set(), set()).oidc_configured());
        assert!(!all(set(), None, set()).oidc_configured());
        assert!(!all(set(), set(), None).oidc_configured());
        assert!(!all(None, None, None).oidc_configured());
    }

    #[test]
    fn an_absent_secret_stays_absent_in_debug_output() {
        // `redact_option` must distinguish "not configured" from "configured
        // but hidden": rendering `None` as `<redacted>` would make an operator
        // debugging a missing token believe it was set.
        let unset = ServerConfig {
            tmdb_api_token: None,
            oidc_client_secret: None,
            ..Default::default()
        };
        let output = format!("{unset:?}");
        assert!(
            output.contains("tmdb_api_token: None"),
            "an unset secret must render as None, got: {output}"
        );

        let set = ServerConfig {
            tmdb_api_token: Some("tmdb-secret-token".to_string()),
            ..Default::default()
        };
        let output = format!("{set:?}");
        // The field itself, not merely a `<redacted>` somewhere in the struct:
        // `database_url` renders one too, so a looser assertion passes even
        // when the secret is dropped entirely.
        assert!(
            output.contains(r#"tmdb_api_token: Some("<redacted>")"#),
            "a set secret must render as redacted, not as absent: {output}"
        );
        assert!(!output.contains("tmdb-secret-token"), "output: {output}");
    }

    #[test]
    fn the_compiled_in_defaults_are_internally_valid() {
        // A defaults-only deployment must start: every declared default has to
        // satisfy the validation the same struct enforces. Asserting each
        // default's *value* would just restate the attribute above it; this
        // asserts the property that actually matters.
        ServerConfig::default()
            .validate_values()
            .expect("a defaults-only configuration must pass its own validation");
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

    mod validate_paths {
        use super::*;

        /// `validate_paths` reaches the real filesystem, and a `TempDir` is a
        /// real filesystem that needs no infrastructure -- so it is the subject
        /// here rather than a `FileSystem` double, which would only prove the
        /// double returns what it was configured to return.
        fn config_in(dir: &std::path::Path) -> ServerConfig {
            ServerConfig {
                video_dir: dir.join("videos"),
                data_dir: dir.join("data"),
                ..Default::default()
            }
        }

        #[test]
        fn a_missing_video_dir_is_rejected() {
            let temp = tempfile::tempdir().unwrap();
            let config = config_in(temp.path());

            let err = config
                .validate_paths()
                .expect_err("a video dir that does not exist must be rejected");
            assert!(
                matches!(&err, ConfigError::DirNotFoundError(path) if path.contains("videos")),
                "unexpected error: {err}"
            );
        }

        #[test]
        fn a_video_dir_that_is_a_file_is_rejected() {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("videos"), b"not a directory").unwrap();

            let err = config_in(temp.path())
                .validate_paths()
                .expect_err("a regular file is not a usable video directory");
            assert!(
                matches!(&err, ConfigError::DirNotFoundError(_)),
                "unexpected error: {err}"
            );
        }

        #[test]
        fn an_existing_video_dir_is_accepted_and_the_data_parent_is_created() {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("videos")).unwrap();
            let config = ServerConfig {
                video_dir: temp.path().join("videos"),
                // Two levels deep: the parent does not exist yet.
                data_dir: temp.path().join("state").join("beam"),
                ..Default::default()
            };

            config
                .validate_paths()
                .expect("an existing video dir and a creatable data dir are valid");

            assert!(
                temp.path().join("state").is_dir(),
                "the data directory's parent must be created, not merely tolerated"
            );
            assert!(
                !temp.path().join("state").join("beam").exists(),
                "only the parent is created; the data dir itself is the server's to make"
            );
        }

        #[test]
        fn a_data_dir_at_the_filesystem_root_needs_no_parent_created() {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("videos")).unwrap();
            let config = ServerConfig {
                video_dir: temp.path().join("videos"),
                // Parent is the temp dir, which already exists.
                data_dir: temp.path().join("data"),
                ..Default::default()
            };

            config.validate_paths().expect("an existing parent is fine");
        }
    }
}
