//! The decisions the process entry point makes before it starts serving.
//!
//! `main.rs` used to hold these inline, which made them unreachable from any
//! test: the cookie-security gate that refuses to start, and the CORS handler
//! every response passes through. Both are policy, and both are the kind of
//! thing that is only noticed when it is wrong in production.

use kynos::middleware::cors::Cors;

use crate::config::{CookieSecurityVerdict, ServerConfig};

/// What the startup cookie-security gate decided.
#[derive(Debug, PartialEq, Eq)]
pub enum StartupGate {
    /// Start normally.
    Proceed,
    /// Start, but log this warning first.
    ProceedWithWarning(String),
}

/// Refuse to start on a cookie configuration that would ship a session cookie
/// without `Secure` on what looks like an HTTPS deployment.
///
/// An explicit `BEAM_COOKIE_SECURE=false` is honoured (with a warning) --
/// somebody who set it deliberately may have a topology that needs it. An
/// *implicit* insecure resolution alongside HTTPS-looking origins is a
/// misconfiguration and stops the process, because on a headless server a
/// warning in a log nobody reads is not a control.
pub fn check_cookie_security(config: &ServerConfig) -> Result<StartupGate, String> {
    match config.cookie_security_verdict() {
        CookieSecurityVerdict::Ok => Ok(StartupGate::Proceed),
        CookieSecurityVerdict::WarnExplicitInsecure => Ok(StartupGate::ProceedWithWarning(
            "BEAM_COOKIE_SECURE=false was set explicitly while BEAM_WEB_URL/\
             BEAM_EXTRA_ALLOWED_ORIGINS suggest an HTTPS deployment -- the session \
             cookie will be issued without the Secure flag. Only keep this override \
             if you understand why your topology needs it."
                .to_string(),
        )),
        CookieSecurityVerdict::ErrLikelyMisconfigured => Err(format!(
            "cookie security misconfiguration: cookies resolved to Secure=false (from \
             BEAM_SERVER_URL={:?}) while BEAM_WEB_URL/BEAM_EXTRA_ALLOWED_ORIGINS suggest an \
             HTTPS deployment. The session cookie would ship without the Secure flag on \
             what looks like a production HTTPS site. Set BEAM_SERVER_URL to the \
             externally-visible HTTPS URL, or set BEAM_COOKIE_SECURE=true (or =false to \
             explicitly accept insecure cookies).",
            config.server_url
        )),
    }
}

/// Environment variables Beam no longer reads, and what replaced each.
const RETIRED_ENV_VARS: &[(&str, &str)] = &[(
    "BEAM_HASH_UNKNOWN_FILES",
    "only video files are indexed now, and every one of them is hashed; \
     subtitles, artwork and other files are no longer given rows (issue #182)",
)];

/// A warning for each retired variable `is_set` reports as still set, so an
/// operator whose deployment still sets one learns it does nothing rather than
/// assuming it still takes effect. Never fatal: the variable is harmless.
pub fn retired_env_warnings(is_set: impl Fn(&str) -> bool) -> Vec<String> {
    RETIRED_ENV_VARS
        .iter()
        .filter(|(name, _)| is_set(name))
        .map(|(name, replacement)| {
            format!("{name} is set but no longer has any effect: {replacement}")
        })
        .collect()
}

/// The browser origins Beam trusts: `BEAM_WEB_URL`, `BEAM_SERVER_URL`, and
/// each entry of `BEAM_EXTRA_ALLOWED_ORIGINS`.
///
/// One list for both halves of the cross-origin control. The CORS policy lets
/// these origins *read* a credentialed response, and
/// `middleware::EnforceSameOrigin` lets them *write*; deriving both from this
/// type is what keeps an origin from being trusted for one and not the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedOrigins(Vec<String>);

impl AllowedOrigins {
    /// No origin at all: every cross-origin browser request is refused.
    ///
    /// What a router built without a configuration gets -- the OpenAPI export
    /// and the route-level tests, none of which send an `Origin`.
    pub fn none() -> Self {
        Self::default()
    }

    /// The origins `config` names, each with any trailing `/` removed, since
    /// an `Origin` header never carries one.
    pub fn from_config(config: &ServerConfig) -> Self {
        let mut origins = vec![
            config.web_url.trim_end_matches('/').to_owned(),
            config.server_url.trim_end_matches('/').to_owned(),
        ];
        if let Some(extra) = &config.extra_allowed_origins {
            origins.extend(
                extra
                    .split(',')
                    .map(|s| s.trim().trim_end_matches('/').to_owned())
                    .filter(|s| !s.is_empty()),
            );
        }
        Self(origins)
    }

    /// Whether `origin` (`scheme://host[:port]`) is one of these.
    pub fn contains(&self, origin: &str) -> bool {
        self.0.iter().any(|allowed| allowed == origin)
    }
}

/// The CORS policy every `/v1` response passes through.
///
/// Credentials are allowed, because the web client authenticates with the
/// `beam_session` cookie, so the origins are an allow-list: only the ones
/// `origins` names get `Access-Control-Allow-Origin` back, and a browser on
/// any other origin cannot read the response. This used to mirror every
/// origin on the grounds that `EnforceSameOrigin` rejects cross-origin
/// writes -- but that check never looks at a safe method, so any site whose
/// requests carry the cookie (a same-site sibling, under `SameSite=Lax`) could
/// read `GET /v1/me`, `/v1/sessions` and the library (issue #348).
///
/// A request with no `Origin` -- a native client, curl, a same-origin `GET` --
/// is not a CORS request and is unaffected.
///
/// The advertised methods are not listed here. Kynos derives them from the
/// operations declared on the matched path, so preflight and the description
/// cannot disagree. The hand-written list this replaced was already wrong: it
/// named GET, POST, PUT, DELETE and OPTIONS but not PATCH, which
/// `/v1/admin/users/{id}` has used since issue #85.
pub fn cors_policy(origins: &AllowedOrigins) -> Cors {
    Cors::new()
        .allow_origins(origins.0.clone())
        .allow_headers([
            "authorization",
            "content-type",
            "accept",
            "x-requested-with",
            // Range requests are how the player seeks; without this the
            // browser drops the header and every seek re-downloads.
            "range",
        ])
        .expose_headers(["accept-ranges", "content-length", "content-range"])
        .allow_credentials()
        // Cache the preflight for 1 hour to reduce noise.
        .max_age(std::time::Duration::from_secs(3600))
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;
