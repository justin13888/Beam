//! Runtime configuration the OIDC flow needs beyond what a single service
//! trait naturally carries.
//!
//! This lived in `beam-auth::server` while that module held Salvo handlers.
//! The Kynos migration moved the HTTP adapter to `beam-server` (ADR-0010), and
//! this struct came here rather than with it: it names no transport type, and
//! the session-TTL arithmetic below is depended on by the session store and by
//! the server's authenticator alike.

/// Seconds in a day. Named because the conversion appeared inline at three
/// call sites, where a `* 24 * 60 * 60` typed as `* 24 + 60 * 60` produces a
/// session that expires in about a minute and a half instead of two weeks --
/// a difference no type checks and, until this was extracted, no test could
/// reach.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Deployment-shaped knobs the OIDC login/callback round-trip reads.
#[derive(Debug, Clone)]
pub struct OidcRuntimeConfig {
    /// Base URL of the web client; the callback redirects here on success.
    pub web_url: String,
    /// Whether to mark cookies `Secure` (derived from the deployment's
    /// scheme; `false` only makes sense for plain-HTTP local dev).
    pub cookie_secure: bool,
    /// Name of the ID-token claim that grants admin (see `admin_claim`).
    /// `None` -> nobody is granted admin at login, and any existing admin is
    /// demoted at their next login (issue #85).
    pub admin_claim: Option<String>,
    /// Expected value for `admin_claim`. `None` -> the claim must assert
    /// boolean `true`; `Some(v)` -> the claim must equal `v` or (if an array)
    /// contain `v`.
    pub admin_value: Option<String>,
    pub session_idle_days: u64,
    pub session_max_days: u64,
}

impl OidcRuntimeConfig {
    /// How long a session survives without activity, in seconds.
    #[allow(clippy::missing_const_for_fn)]
    pub fn idle_ttl_secs(&self) -> u64 {
        self.session_idle_days * SECONDS_PER_DAY
    }

    /// Hard ceiling on session lifetime from creation, in seconds.
    pub fn absolute_ttl_secs(&self) -> u64 {
        self.session_max_days * SECONDS_PER_DAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(idle: u64, max: u64) -> OidcRuntimeConfig {
        OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_owned(),
            cookie_secure: false,
            admin_claim: None,
            admin_value: None,
            session_idle_days: idle,
            session_max_days: max,
        }
    }

    /// The arithmetic this constant exists to protect: 14 days is 1_209_600
    /// seconds, and `* 24 + 60 * 60` would yield 3_936.
    #[test]
    fn idle_ttl_converts_days_to_seconds() {
        assert_eq!(config(14, 90).idle_ttl_secs(), 1_209_600);
    }

    #[test]
    fn absolute_ttl_converts_days_to_seconds() {
        assert_eq!(config(14, 90).absolute_ttl_secs(), 7_776_000);
    }
}
