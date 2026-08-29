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

/// The CORS policy every `/v1` response passes through.
///
/// The origin is mirrored rather than allow-listed, and credentials are
/// allowed -- which on its own would let any site read an authenticated
/// response. It is safe here only because `/v1` sits behind
/// `middleware::enforce_same_origin`, which rejects a cross-origin request
/// before it reaches a handler. The two are a pair; neither is sufficient
/// alone.
///
/// Two things changed with the Kynos migration:
///
/// * Mirroring is spelled `allow_origins_matching(|_| true)` rather than
///   `AllowOrigin::mirror_request()`. Kynos refuses `allow_any_origin()`
///   alongside `allow_credentials()` while the router is built, because the
///   CORS protocol forbids `Access-Control-Allow-Origin: *` on a credentialed
///   response -- a combination Salvo would have emitted for browsers to reject.
/// * The advertised methods are no longer listed here. Kynos derives them from
///   the operations declared on the matched path, so preflight and the
///   description cannot disagree. The hand-written list this replaces was
///   already wrong: it named GET, POST, PUT, DELETE and OPTIONS but not PATCH,
///   which `/v1/admin/users/{id}` has used since issue #85.
pub fn cors_policy() -> Cors {
    Cors::new()
        .allow_origins_matching(|_| true)
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
