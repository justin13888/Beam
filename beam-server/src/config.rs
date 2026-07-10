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
}

/// Application configuration
#[derive(Clone, Config)]
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

    /// Whether to apply pending database migrations at startup. On by
    /// default so a container-only deployment needs no separate migration
    /// step; disable for operator-managed migrations via the
    /// `beam-migration` CLI (`up`/`down`/`status`).
    #[config(env = "BEAM_AUTO_MIGRATE", default = true)]
    pub auto_migrate: bool,

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
            video_dir,
            cache_dir,
            database_url,
            auto_migrate,
            hash_unknown_files,
            scan_interval_secs,
            watch_enabled,
            watch_debounce_ms,
            enrich_interval_secs,
            tmdb_api_token,
            anilist_enabled,
            oidc_issuer,
            oidc_client_id,
            oidc_client_secret,
            oidc_scopes,
            web_url,
            extra_allowed_origins,
            admin_emails,
            cookie_secure,
            session_idle_days,
            session_max_days,
        } = self;
        f.debug_struct("ServerConfig")
            .field("bind_address", bind_address)
            .field("server_url", server_url)
            .field("enable_metrics", enable_metrics)
            .field("video_dir", video_dir)
            .field("cache_dir", cache_dir)
            .field("database_url", &redact_url_password(database_url))
            .field("auto_migrate", auto_migrate)
            .field("hash_unknown_files", hash_unknown_files)
            .field("scan_interval_secs", scan_interval_secs)
            .field("watch_enabled", watch_enabled)
            .field("watch_debounce_ms", watch_debounce_ms)
            .field("enrich_interval_secs", enrich_interval_secs)
            .field("tmdb_api_token", &redact_option(tmdb_api_token))
            .field("anilist_enabled", anilist_enabled)
            .field("oidc_issuer", oidc_issuer)
            .field("oidc_client_id", oidc_client_id)
            .field("oidc_client_secret", &redact_option(oidc_client_secret))
            .field("oidc_scopes", oidc_scopes)
            .field("web_url", web_url)
            .field("extra_allowed_origins", extra_allowed_origins)
            .field("admin_emails", admin_emails)
            .field("cookie_secure", cookie_secure)
            .field("session_idle_days", session_idle_days)
            .field("session_max_days", session_max_days)
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
            video_dir: PathBuf::from("/videos"),
            cache_dir: PathBuf::from("/cache"),
            database_url: "postgres://beam:db-secret-pw@localhost:5432/beam".to_string(),
            auto_migrate: true,
            hash_unknown_files: true,
            scan_interval_secs: 3600,
            watch_enabled: true,
            watch_debounce_ms: 2000,
            enrich_interval_secs: 300,
            tmdb_api_token: Some("tmdb-secret-token".to_string()),
            anilist_enabled: true,
            oidc_issuer: Some("https://idp.example.com".to_string()),
            oidc_client_id: Some("beam-client".to_string()),
            oidc_client_secret: Some("oidc-secret-value".to_string()),
            oidc_scopes: "openid profile email".to_string(),
            web_url: "https://beam.example.com".to_string(),
            extra_allowed_origins: None,
            admin_emails: Some("admin@example.com".to_string()),
            cookie_secure: None,
            session_idle_days: 14,
            session_max_days: 60,
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
