use beam_auth::utils::oidc::ClientAuthMethod;
use confique::Config;
use kynos::http::forwarded::{Network, ProxyHeader, TrustedProxies};
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

    /// Glob patterns, comma-separated, for paths under a library root the
    /// indexer must leave alone (issue #182) -- in addition to the hidden
    /// files, NAS housekeeping folders, extras and samples it always skips.
    /// Each is matched case-insensitively against the path relative to the
    /// library root; `*` stops at a `/`, `**` crosses it, and a pattern that
    /// matches a directory skips everything beneath it: `Downloads`,
    /// `**/*.partial.mkv`, `Kids/**`. An invalid pattern is a startup error.
    #[config(
        env = "BEAM_SCAN_IGNORE",
        parse_env = confique::env::parse::list_by_comma,
        default = []
    )]
    pub scan_ignore: Vec<String>,

    /// Interval between periodic full rescans of every library, in seconds.
    /// Acts as the backstop that catches changes the filesystem watcher missed.
    #[config(env = "BEAM_SCAN_INTERVAL_SECS", default = 3600)]
    pub scan_interval_secs: u64,

    /// How many days a file may stay missing from disk before a scan purges
    /// its row -- and, with it, every user's playback progress for it. Until
    /// then the file is only hidden, and comes back under the same id if its
    /// path reappears (an unmounted NAS or USB disk remounted). `0` purges at
    /// the first healthy scan that does not find the file.
    #[config(env = "BEAM_MISSING_FILE_GRACE_DAYS", default = 30)]
    pub missing_file_grace_days: u32,

    /// How many seconds a file must go without a write before the indexer
    /// hashes and probes it. A file still being copied into a library is
    /// left alone until then -- the scan counts it as deferred, and a
    /// watcher event retries it -- instead of being hashed again after every
    /// burst of writes, and probed while its container index is missing.
    /// `0` hashes every file on sight.
    #[config(env = "BEAM_SCAN_SETTLE_SECS", default = 30)]
    pub scan_settle_secs: u64,

    /// Whether to run the filesystem watcher for near-real-time index
    /// updates. When false, only the startup scan and periodic rescans run.
    #[config(env = "BEAM_WATCH_ENABLED", default = true)]
    pub watch_enabled: bool,

    /// Debounce window for filesystem-watcher events, in milliseconds. Bursts
    /// of events for the same path within this window collapse into one.
    #[config(env = "BEAM_WATCH_DEBOUNCE_MS", default = 2000)]
    pub watch_debounce_ms: u64,

    /// Interval between walks of the libraries the watcher polls instead of
    /// watching natively -- roots on a network filesystem, and libraries past
    /// the OS watch limit -- in seconds. Each poll walks every such library,
    /// so lower means fresher and more I/O. Must be at least 1.
    #[config(env = "BEAM_WATCH_POLL_INTERVAL_SECS", default = 300)]
    pub watch_poll_interval_secs: u64,

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

    /// TheTVDB v4 API key, for TheTVDB-sourced enrichment (issue #267):
    /// series with their specials and absolute episode numbering, and
    /// movies. If absent, TheTVDB is not asked, and nothing else changes.
    #[config(env = "BEAM_TVDB_API_KEY")]
    pub tvdb_api_key: Option<String>,

    /// The subscriber PIN TheTVDB issues with a user-supported API key.
    /// Unset for a licensed project key, which has none.
    #[config(env = "BEAM_TVDB_PIN")]
    pub tvdb_pin: Option<String>,

    /// Preferred language for provider metadata, as a BCP-47 tag like `en`
    /// or `en-US` (lowercase language, uppercase region). Applied to the
    /// TMDB client only -- AniList has no language concept. Unset -> the
    /// provider's own default. An empty or whitespace-only value is
    /// normalized to unset (confique parses a set-but-empty env var as
    /// `Some("")`).
    #[config(env = "BEAM_METADATA_LANGUAGE")]
    pub metadata_language: Option<String>,

    /// Ceiling for the on-disk artwork cache, in bytes. Beam serves poster
    /// and backdrop art itself (ADR-0015), caching it under
    /// `BEAM_DATA_DIR/artwork`; once the total exceeds this, the least
    /// recently served entries are deleted. The resized renditions clients
    /// ask for with `?size=` are entries of the same cache under the same
    /// ceiling. Sizing it below a library's artwork costs re-fetches and
    /// re-resizes, never correctness.
    #[config(env = "BEAM_ARTWORK_CACHE_MAX_BYTES", default = 1_073_741_824)]
    pub artwork_cache_max_bytes: u64,

    /// How long Beam waits on a provider CDN for one image, in seconds.
    #[config(env = "BEAM_ARTWORK_FETCH_TIMEOUT_SECS", default = 10)]
    pub artwork_fetch_timeout_secs: u64,

    /// Largest single image Beam will accept from a provider, in bytes. A
    /// response over this is refused rather than buffered.
    #[config(env = "BEAM_ARTWORK_MAX_IMAGE_BYTES", default = 16_777_216)]
    pub artwork_max_image_bytes: u64,

    /// How long a failed artwork fetch is remembered, in seconds, so a
    /// provider that is down (or art that has been deleted upstream) is not
    /// re-requested once per client per grid render.
    #[config(env = "BEAM_ARTWORK_NEGATIVE_TTL_SECS", default = 300)]
    pub artwork_negative_ttl_secs: u64,

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

    /// How Beam's client authenticates to the IdP, as its registration there
    /// names it: `client_secret_basic` (HTTP Basic, the default) or
    /// `client_secret_post` (the secret in the form body) -- RFC 7591's
    /// `token_endpoint_auth_method` values. Applies to the authorization-code
    /// exchange and both device-grant requests alike. An IdP accepts only the
    /// method a client is registered with and publishes that nowhere, so it
    /// is set here rather than guessed (ADR-0017). Any other value is a
    /// startup error; an empty one is unset, so it means the default.
    #[config(env = "BEAM_OIDC_CLIENT_AUTH_METHOD", default = "client_secret_basic")]
    pub oidc_client_auth_method: ClientAuthMethod,

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

    /// Sustained request rate — and burst capacity — for the endpoints that
    /// begin a login (`/v1/auth/login`, `/v1/auth/callback`,
    /// `/v1/auth/device`), per client key, in requests per minute. Must be at
    /// least 1.
    #[config(env = "BEAM_RATE_LIMIT_AUTH_PER_MINUTE", default = 10)]
    pub rate_limit_auth_per_minute: u32,

    /// Sustained request rate — and burst capacity — for the media
    /// browse/search endpoint (`GET /v1/media`), per client key, in requests
    /// per minute. Must be at least 1.
    #[config(env = "BEAM_RATE_LIMIT_SEARCH_PER_MINUTE", default = 60)]
    pub rate_limit_search_per_minute: u32,

    /// Sustained request rate — and burst capacity — for device-login polls
    /// (`POST /v1/auth/device/token`), per client key, in requests per
    /// minute. Must be at least 1. Its own budget because a waiting TV polls
    /// every few seconds for minutes -- far more than the login budget allows
    /// -- while each flow's own interval (ADR-0017) already paces what reaches
    /// the IdP.
    #[config(env = "BEAM_RATE_LIMIT_DEVICE_POLL_PER_MINUTE", default = 30)]
    pub rate_limit_device_poll_per_minute: u32,

    /// Whether to believe the one `X-Forwarded-For` hop nearest the server,
    /// whatever its address -- for a managed load balancer whose addresses are
    /// not known in advance. Off by default: the header is trivially spoofable
    /// unless the server sits behind a proxy that appends to it. When off, and
    /// [`Self::trusted_proxies`] is empty, the peer socket IP is used. Like
    /// `BEAM_TRUSTED_PROXIES`, it governs both the rate-limit client key and
    /// the address recorded on a session.
    #[config(env = "BEAM_RATE_LIMIT_TRUST_FORWARDED_FOR", default = false)]
    pub rate_limit_trust_forwarded_for: bool,

    /// The reverse proxies whose `X-Forwarded-For` is believed, as
    /// comma-separated CIDR networks (`10.0.0.0/8, 2001:db8::/32`; one host is
    /// `/32` or `/128`). **Empty -- the default -- believes nobody**: the
    /// client address is the socket peer, whatever the request's headers say.
    /// A request whose peer is in one of these networks is attributed to the
    /// address that proxy appended, walking back through any further listed
    /// proxies. Governs both the address recorded on a session and the rate
    /// limiter's client key (issue #200). `/0` is refused; an entry that is
    /// not a network is a startup error.
    #[config(
        env = "BEAM_TRUSTED_PROXIES",
        parse_env = confique::env::parse::list_by_comma,
        default = []
    )]
    pub trusted_proxies: Vec<String>,

    /// Where to send the anonymous weekly library report (issue #93,
    /// ADR-0019): an OTLP/HTTP metrics endpoint, used exactly as given --
    /// usually ending `/v1/metrics`. **Unset -- the default -- sends nothing,
    /// ever**; there is no built-in collector. The report holds aggregate
    /// counts only, each as a coarse range (titles, files, containers,
    /// codecs, size buckets, server version), and never a title, path, name
    /// or identifier. An admin can see
    /// the exact bytes at `GET /v1/admin/telemetry/library` before opting in.
    /// Must be an `http` or `https` URL with a host; an empty value is unset.
    /// Credentials for the collector go in the URL: a query token, or
    /// userinfo, sent as a Basic `Authorization` header. Only the origin is
    /// ever logged.
    #[config(env = "BEAM_TELEMETRY_URL")]
    pub telemetry_url: Option<String>,

    /// Whether clients may report playback starts, failures, rebuffers and
    /// source switches (issue #143, ADR-0019). **Off by default.** When on,
    /// the server keeps daily counts per client kind, container, codec and
    /// resolution class -- never a user, file or title -- for admins to read
    /// at `GET /v1/admin/telemetry/playback`. Nothing leaves the server:
    /// these counts are not part of the `BEAM_TELEMETRY_URL` report. Turning
    /// it off stops collection; what was already counted stays until the
    /// retention period removes it.
    #[config(env = "BEAM_PLAYBACK_TELEMETRY_ENABLED", default = false)]
    pub playback_telemetry_enabled: bool,

    /// How many days playback counts are kept after the day they count
    /// (1 to 3650). Pruned daily whether or not collection is enabled.
    #[config(env = "BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS", default = 365)]
    pub playback_telemetry_retention_days: u32,
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
            scan_ignore,
            scan_interval_secs,
            missing_file_grace_days,
            scan_settle_secs,
            watch_enabled,
            watch_debounce_ms,
            watch_poll_interval_secs,
            enrich_interval_secs,
            enrich_batch_size,
            enrich_min_confidence,
            tmdb_api_token,
            anilist_enabled,
            tvdb_api_key,
            tvdb_pin,
            metadata_language,
            artwork_cache_max_bytes,
            artwork_fetch_timeout_secs,
            artwork_max_image_bytes,
            artwork_negative_ttl_secs,
            oidc_issuer,
            oidc_client_id,
            oidc_client_secret,
            oidc_client_auth_method,
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
            rate_limit_device_poll_per_minute,
            rate_limit_trust_forwarded_for,
            trusted_proxies,
            telemetry_url,
            playback_telemetry_enabled,
            playback_telemetry_retention_days,
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
            .field("scan_ignore", scan_ignore)
            .field("scan_interval_secs", scan_interval_secs)
            .field("missing_file_grace_days", missing_file_grace_days)
            .field("scan_settle_secs", scan_settle_secs)
            .field("watch_enabled", watch_enabled)
            .field("watch_debounce_ms", watch_debounce_ms)
            .field("watch_poll_interval_secs", watch_poll_interval_secs)
            .field("enrich_interval_secs", enrich_interval_secs)
            .field("enrich_batch_size", enrich_batch_size)
            .field("enrich_min_confidence", enrich_min_confidence)
            .field("tmdb_api_token", &redact_option(tmdb_api_token))
            .field("anilist_enabled", anilist_enabled)
            .field("tvdb_api_key", &redact_option(tvdb_api_key))
            .field("tvdb_pin", &redact_option(tvdb_pin))
            .field("metadata_language", metadata_language)
            .field("artwork_cache_max_bytes", artwork_cache_max_bytes)
            .field("artwork_fetch_timeout_secs", artwork_fetch_timeout_secs)
            .field("artwork_max_image_bytes", artwork_max_image_bytes)
            .field("artwork_negative_ttl_secs", artwork_negative_ttl_secs)
            .field("oidc_issuer", oidc_issuer)
            .field("oidc_client_id", oidc_client_id)
            .field("oidc_client_secret", &redact_option(oidc_client_secret))
            .field("oidc_client_auth_method", oidc_client_auth_method)
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
                "rate_limit_device_poll_per_minute",
                rate_limit_device_poll_per_minute,
            )
            .field(
                "rate_limit_trust_forwarded_for",
                rate_limit_trust_forwarded_for,
            )
            .field("trusted_proxies", trusted_proxies)
            // Origin only: a collector URL may carry an ingest token in its
            // path or query, and the log line must not.
            .field(
                "telemetry_url",
                &telemetry_url
                    .as_deref()
                    .map(|url| url_origin(url).unwrap_or_else(|| "<invalid>".to_string())),
            )
            .field("playback_telemetry_enabled", playback_telemetry_enabled)
            .field(
                "playback_telemetry_retention_days",
                playback_telemetry_retention_days,
            )
            .finish()
    }
}

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// The retention `BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS` accepts: at least a
/// day, at most ten years.
pub const PLAYBACK_TELEMETRY_RETENTION_DAYS: std::ops::RangeInclusive<u32> = 1..=3650;

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

/// `scheme://host[:port]` of an `http`/`https` URL with a host, or `None`
/// for anything else. The one parse both validation and redaction use, so a
/// URL the server accepts is always one it can print safely.
pub(crate) fn url_origin(raw: &str) -> Option<String> {
    let uri: http::Uri = raw.parse().ok()?;
    let scheme = uri.scheme_str()?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = uri.host().filter(|host| !host.is_empty())?;
    Some(match uri.port_u16() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
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

    /// How long a file may stay missing before a scan purges it (issue #179).
    pub fn missing_file_grace(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.missing_file_grace_days) * SECONDS_PER_DAY)
    }

    /// Which paths under a library root the indexer indexes: the built-in
    /// rules plus [`Self::scan_ignore`].
    pub fn scan_path_policy(
        &self,
    ) -> Result<beam_domain::utils::path_policy::PathPolicy, ConfigError> {
        beam_domain::utils::path_policy::PathPolicy::new(&self.scan_ignore).map_err(|err| {
            ConfigError::InvalidValue("BEAM_SCAN_IGNORE".to_string(), err.to_string())
        })
    }

    /// Which hops the router believes about who the client is: the networks
    /// in [`Self::trusted_proxies`], plus the one nearest hop
    /// [`Self::rate_limit_trust_forwarded_for`] trusts wherever it is.
    ///
    /// Both read `X-Forwarded-For` alone -- the field a reverse proxy appends
    /// to -- so a `Forwarded` the client wrote itself is never believed.
    /// Trusting nobody, the default, means the socket peer.
    ///
    /// One policy for the whole router: the session's recorded address and the
    /// rate limiter's client key are always the same address.
    pub fn trusted_proxy_policy(&self) -> Result<TrustedProxies, ConfigError> {
        let networks = self
            .trusted_proxies
            .iter()
            .map(|entry| {
                entry.parse::<Network>().map_err(|reason| {
                    ConfigError::InvalidValue(
                        "BEAM_TRUSTED_PROXIES".to_string(),
                        format!("{entry:?} is not a network to trust: {reason}"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if self.rate_limit_trust_forwarded_for {
            TrustedProxies::hops(ProxyHeader::XForwarded, 1).and_networks(networks)
        } else {
            TrustedProxies::networks(ProxyHeader::XForwarded, networks)
        })
    }

    /// What the anonymous library report is scheduled with (issue #93): the
    /// collector, its printable origin, and where deliveries are recorded --
    /// `BEAM_DATA_DIR/telemetry/library-report.json`.
    pub fn library_report_config(&self) -> crate::services::telemetry::LibraryReportConfig {
        crate::services::telemetry::LibraryReportConfig {
            destination: self.telemetry_url.clone(),
            destination_origin: self.telemetry_url.as_deref().and_then(url_origin),
            state_path: self.data_dir.join("telemetry").join("library-report.json"),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// What the playback telemetry service is configured with (issue #143).
    pub fn playback_telemetry_config(
        &self,
    ) -> crate::services::playback_telemetry::PlaybackTelemetryConfig {
        crate::services::playback_telemetry::PlaybackTelemetryConfig {
            enabled: self.playback_telemetry_enabled,
            retention_days: self.playback_telemetry_retention_days,
        }
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
        self.tvdb_api_key = empty_to_none(self.tvdb_api_key.take());
        self.tvdb_pin = empty_to_none(self.tvdb_pin.take());
        // `list_by_comma` splits without trimming: `a, b` is `["a", " b"]`,
        // and a trailing comma leaves an empty pattern.
        self.scan_ignore = std::mem::take(&mut self.scan_ignore)
            .into_iter()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty())
            .collect();
        self.trusted_proxies = std::mem::take(&mut self.trusted_proxies)
            .into_iter()
            .map(|network| network.trim().to_string())
            .filter(|network| !network.is_empty())
            .collect();
        self.oidc_admin_claim = empty_to_none(self.oidc_admin_claim.take());
        self.oidc_admin_value = empty_to_none(self.oidc_admin_value.take());
        self.telemetry_url = empty_to_none(self.telemetry_url.take());
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

        if self.rate_limit_device_poll_per_minute == 0 {
            return Err(ConfigError::InvalidValue(
                "BEAM_RATE_LIMIT_DEVICE_POLL_PER_MINUTE".to_string(),
                "must be at least 1".to_string(),
            ));
        }

        // Zero would poll in a tight loop, walking every polled library
        // back to back.
        if self.watch_poll_interval_secs == 0 {
            return Err(ConfigError::InvalidValue(
                "BEAM_WATCH_POLL_INTERVAL_SECS".to_string(),
                "must be at least 1".to_string(),
            ));
        }

        self.scan_path_policy()?;
        self.trusted_proxy_policy()?;

        // An expected admin-claim value is meaningless without the claim to
        // read it from -- reject the half-configured combination up front
        // rather than silently granting admin to nobody.
        if self.oidc_admin_value.is_some() && self.oidc_admin_claim.is_none() {
            return Err(ConfigError::InvalidValue(
                "BEAM_OIDC_ADMIN_VALUE".to_string(),
                "requires BEAM_OIDC_ADMIN_CLAIM to also be set".to_string(),
            ));
        }

        // Refused rather than ignored: an operator who set this meant to opt
        // in, and a report that silently never leaves is not what they chose.
        if let Some(url) = &self.telemetry_url
            && url_origin(url).is_none()
        {
            return Err(ConfigError::InvalidValue(
                "BEAM_TELEMETRY_URL".to_string(),
                "must be an http or https URL with a host".to_string(),
            ));
        }

        if !PLAYBACK_TELEMETRY_RETENTION_DAYS.contains(&self.playback_telemetry_retention_days) {
            return Err(ConfigError::InvalidValue(
                "BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS".to_string(),
                format!(
                    "must be in the range {}..={}, got {}",
                    PLAYBACK_TELEMETRY_RETENTION_DAYS.start(),
                    PLAYBACK_TELEMETRY_RETENTION_DAYS.end(),
                    self.playback_telemetry_retention_days
                ),
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
#[path = "config_tests.rs"]
mod tests;
