//! CSRF defense-in-depth for cookie-authenticated requests (see ADR-0003).
//!
//! `SameSite=Lax` on `beam_session` is the primary defense: it already
//! blocks the browser from attaching the cookie to a cross-site POST/PUT/
//! DELETE at all. This interceptor is a second layer for the cases that aren't
//! airtight (older browsers, a misconfigured intermediary) -- state-changing
//! requests must present an `Origin` (or `Referer`, as a fallback) matching
//! an allowed origin. A request with neither header is let through: some
//! legitimate non-browser clients (curl, mobile apps) send neither, and
//! those never carry the cookie in the first place -- SameSite is what
//! actually stops a browser-based attack.
//!
//! Under Salvo this was a `#[handler]` hoop, and its 403 existed only at run
//! time -- issue #123 recorded it as one of the statuses missing from the
//! emitted spec entirely. A Kynos `Interceptor` declares what it answers with
//! and what it reads, so both now appear on every operation it covers.

use kynos::error::rejection::HeaderRejection;
use kynos::extract::params::header::{DecodeHeaders, HeaderParams};
use kynos::http::{HeaderMap, Method, Request};
use kynos::middleware::{Continued, Interceptor, Next};
use kynos::prelude::*;

use crate::bootstrap::AllowedOrigins;
use crate::state::AppState;

/// Extracts `scheme://host[:port]` from an Origin or Referer header value,
/// discarding any path/query (Referer includes them; Origin never does).
fn extract_origin(value: &str) -> Option<String> {
    let scheme_end = value.find("://")? + 3;
    let authority_end = value[scheme_end..]
        .find('/')
        .map_or(value.len(), |i| scheme_end + i);
    Some(value[..authority_end].to_owned())
}

/// The two headers this check reads.
///
/// Declaring them is how it gets them: `Reads` arrives already extracted, so an
/// interceptor cannot claim a parameter it never looks at, nor read one it did
/// not declare.
///
/// Declared but not *described*: a browser sets both on its own, so they are
/// transport, not an input any client chooses, and describing them would put
/// `Origin` and `Referer` on every operation this interceptor covers and in
/// every generated client's parameters (issue #200).
//
// Hand-implemented rather than derived because `#[derive(HeaderParams)]` in
// kynos 0.4.0 has no way to set `DESCRIBED = false`: getkono/kynos#1021.
// Once a release carries that attribute, go back to the derive with it.
#[derive(Debug)]
pub struct OriginHeaders {
    pub origin: Option<String>,
    pub referer: Option<String>,
}

impl HeaderParams for OriginHeaders {
    const NAMES: &'static [&'static str] = &["Origin", "Referer"];
    const DESCRIBED: bool = false;
}

impl DecodeHeaders for OriginHeaders {
    fn decode(headers: &HeaderMap) -> Result<Self, HeaderRejection> {
        Ok(Self {
            origin: optional_header(headers, "Origin")?,
            referer: optional_header(headers, "Referer")?,
        })
    }
}

/// The first value of the optional header `name`, refused as the derive would
/// refuse it when it is not printable ASCII.
pub(crate) fn optional_header(
    headers: &HeaderMap,
    name: &str,
) -> Result<Option<String>, HeaderRejection> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| HeaderRejection::Invalid {
                    name: name.to_owned(),
                    detail: "the value is not printable ASCII".to_owned(),
                })
        })
        .transpose()
}

/// The 403 a cross-origin state-changing request gets.
///
/// A named type because `Short` is the only way an interceptor can answer
/// without reaching the handler, and its `Responses` is what the document
/// prints.
///
/// `NotAllowed` is written first. Kynos describes a status by joining the
/// title of every declaration of it in the order it meets them, so on every
/// operation this interceptor covers the 403 reads "Forbidden; Cross-origin
/// request rejected; Malformed Origin/Referer header", and the broader of the
/// two refusals comes before the narrower one.
#[derive(Debug, thiserror::Error, kynos::ApiError)]
pub enum CrossOriginRejected {
    #[error("Cross-origin request rejected")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/#cross-origin-rejected",
        title = "Cross-origin request rejected"
    )]
    NotAllowed,

    #[error("Malformed Origin/Referer header")]
    #[problem(
        status = 403,
        type = "https://beam.justinchung.net/reference/errors/#malformed-origin",
        title = "Malformed Origin/Referer header"
    )]
    Malformed,
}

/// CSRF defense-in-depth for cookie-authenticated state-changing requests.
pub struct EnforceSameOrigin;

impl Interceptor<AppState> for EnforceSameOrigin {
    type Reads = OriginHeaders;
    type Adds = ();
    type Short = CrossOriginRejected;

    async fn intercept(
        &self,
        request: Request,
        reads: OriginHeaders,
        context: &AppState,
        next: Next<'_, AppState>,
    ) -> Result<Continued<()>, CrossOriginRejected> {
        // A safe method changes no state, so there is nothing to forge.
        if matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
        ) {
            return Ok(next.run(request).await);
        }

        let OriginHeaders { origin, referer } = reads;
        let Some(raw_origin) = origin.or(referer) else {
            // No Origin/Referer at all -- allow. SameSite=Lax already stops the
            // browser cross-site case; this header is only ever absent for
            // non-browser clients that don't carry the cookie anyway.
            return Ok(next.run(request).await);
        };

        let origin = extract_origin(&raw_origin).ok_or(CrossOriginRejected::Malformed)?;

        if AllowedOrigins::from_config(&context.config).contains(&origin) {
            Ok(next.run(request).await)
        } else {
            Err(CrossOriginRejected::NotAllowed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_origin_from_bare_origin_header() {
        assert_eq!(
            extract_origin("http://localhost:5173"),
            Some("http://localhost:5173".to_owned())
        );
    }

    #[test]
    fn extracts_origin_from_referer_with_path() {
        assert_eq!(
            extract_origin("http://localhost:5173/library/some/deep/path?x=1"),
            Some("http://localhost:5173".to_owned())
        );
    }

    #[test]
    fn rejects_malformed_value_without_scheme_separator() {
        assert_eq!(extract_origin("not-a-url"), None);
    }

    #[test]
    fn does_not_prefix_match_a_lookalike_host() {
        // A naive `starts_with` check would let this slip through; exact
        // equality on the extracted origin must not.
        let evil = extract_origin("http://localhost:5173.evil.example/path").unwrap();
        assert_ne!(evil, "http://localhost:5173");
    }
}

#[cfg(test)]
#[path = "middleware_tests.rs"]
mod enforcement_tests;
