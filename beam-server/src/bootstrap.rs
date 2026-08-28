//! The decisions the process entry point makes before it starts serving.
//!
//! `main.rs` used to hold these inline, which made them unreachable from any
//! test: the cookie-security gate that refuses to start, and the CORS handler
//! every response passes through. Both are policy, and both are the kind of
//! thing that is only noticed when it is wrong in production.

use http::Method;
use salvo::cors::Cors;
use salvo::prelude::*;

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

/// The CORS handler every response passes through.
///
/// The origin is mirrored rather than allow-listed, and credentials are
/// allowed -- which on its own would let any site read an authenticated
/// response. It is safe here only because `/v1` sits behind
/// `middleware::enforce_same_origin`, which rejects a cross-origin request
/// before it reaches a handler. The two are a pair; neither is sufficient
/// alone.
pub fn cors_handler() -> impl Handler {
    Cors::new()
        .allow_origin(salvo::cors::AllowOrigin::mirror_request())
        .allow_methods(vec![
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(vec![
            "authorization",
            "content-type",
            "accept",
            "x-requested-with",
            // Range requests are how the player seeks; without this the
            // browser drops the header and every seek re-downloads.
            "range",
        ])
        .expose_headers(vec!["accept-ranges", "content-length", "content-range"])
        .allow_credentials(true)
        .max_age(3600) // Cache the preflight for 1 hour to reduce noise
        .into_handler()
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;
