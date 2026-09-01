//! `reqwest`-backed [`ArtworkFetcher`] adapter.
//!
//! Lives here rather than in beam-server for the same reason the cameo
//! adapter does: this crate already owns every outbound provider call, and
//! beam-server has no HTTP client of its own to grow.
//!
//! The transport glue is deliberately thin, and the decisions it makes are
//! pulled out into [`checked_url`], [`classify`] and [`artwork_request`] --
//! pure functions with no socket in them, which is what lets the policy be
//! tested exhaustively without a TLS listener. Everything above this adapter
//! substitutes `InMemoryArtworkFetcher` instead.

use std::time::Duration;

use beam_domain::providers::artwork::{
    ArtworkFetchError, ArtworkFetcher, FetchedImage, ImageFormat,
};
use reqwest::{Client, Request, Url};
use tracing::warn;

/// How much patience and how much memory one artwork fetch may spend.
#[derive(Debug, Clone, Copy)]
pub struct ArtworkFetchLimits {
    pub timeout: Duration,
    pub max_bytes: u64,
}

/// How many redirects a provider CDN may take before Beam gives up.
const MAX_REDIRECTS: usize = 3;

/// The `Accept` Beam offers a provider, derived from the formats it is
/// actually willing to store so the two cannot drift apart.
fn accept_header() -> String {
    ImageFormat::ALL
        .iter()
        .map(|format| format.content_type())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses a stored artwork URL, refusing anything Beam will not fetch.
///
/// `https` only. Enrichment writes these values, so a cleartext URL means
/// either a provider downgrade or a tampered row, and neither is worth
/// fetching a viewer's artwork over.
pub(crate) fn checked_url(raw: &str) -> Result<Url, ArtworkFetchError> {
    let url = Url::parse(raw).map_err(|err| ArtworkFetchError::Refused {
        url: raw.to_string(),
        reason: err.to_string(),
    })?;
    if url.scheme() != "https" {
        return Err(ArtworkFetchError::Refused {
            url: raw.to_string(),
            reason: format!("scheme {} is not https", url.scheme()),
        });
    }
    Ok(url)
}

/// The outbound request, headers and all.
///
/// Separate from sending it so a test can inspect exactly what Beam would put
/// on the wire without a server to put it on -- which is how NFR-502 (no Beam
/// cookie, header or other first-party credential ever reaches a provider) is
/// held to rather than merely intended.
pub(crate) fn artwork_request(client: &Client, url: Url) -> Result<Request, ArtworkFetchError> {
    client
        .get(url)
        .header(reqwest::header::ACCEPT, accept_header())
        .build()
        .map_err(|err| ArtworkFetchError::Transport(err.to_string()))
}

/// What a response's status and headers decide, before a byte of body is read.
pub(crate) fn classify(
    status: u16,
    content_type: Option<&str>,
    content_length: Option<u64>,
    max_bytes: u64,
) -> Result<ImageFormat, ArtworkFetchError> {
    match status {
        200..=299 => {}
        404 | 410 => return Err(ArtworkFetchError::NotFound),
        other => return Err(ArtworkFetchError::Upstream { status: other }),
    }

    // A provider that answers with an HTML error page under a 200 is the
    // realistic case here, and it must not become a cached "poster".
    let declared = content_type.unwrap_or_default();
    let format =
        ImageFormat::from_content_type(declared).ok_or_else(|| ArtworkFetchError::Unsupported {
            content_type: declared.to_string(),
        })?;

    // Refuse on the declared length before reading; the body loop enforces the
    // same ceiling again for a provider that omits or understates it.
    if content_length.is_some_and(|length| length > max_bytes) {
        return Err(ArtworkFetchError::TooLarge { limit: max_bytes });
    }

    Ok(format)
}

/// [`ArtworkFetcher`] over a `reqwest` client.
#[derive(Debug)]
pub struct ReqwestArtworkFetcher {
    client: Client,
    max_bytes: u64,
}

impl ReqwestArtworkFetcher {
    /// Builds the client Beam fetches provider artwork with.
    ///
    /// No cookie store -- the `cookies` feature is off in the manifest, so
    /// there is no jar for a session cookie to be attached from even by
    /// accident. Redirects are followed only while they stay on `https`, so a
    /// CDN cannot walk Beam down to cleartext.
    pub fn new(limits: ArtworkFetchLimits) -> Result<Self, reqwest::Error> {
        let redirect = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.stop()
            } else if attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });

        let client = Client::builder()
            .timeout(limits.timeout)
            .redirect(redirect)
            .user_agent(concat!("Beam/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            client,
            max_bytes: limits.max_bytes,
        })
    }
}

#[async_trait::async_trait]
impl ArtworkFetcher for ReqwestArtworkFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedImage, ArtworkFetchError> {
        let request = artwork_request(&self.client, checked_url(url)?)?;

        let mut response = self.client.execute(request).await.map_err(|err| {
            warn!(%url, %err, "artwork fetch failed");
            ArtworkFetchError::Transport(err.to_string())
        })?;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        let format = classify(
            response.status().as_u16(),
            content_type.as_deref(),
            response.content_length(),
            self.max_bytes,
        )?;

        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| ArtworkFetchError::Transport(err.to_string()))?
        {
            // Checked per chunk rather than after the fact, so a provider that
            // lied about (or omitted) Content-Length cannot make Beam buffer
            // an unbounded body first and object second.
            if bytes.len() as u64 + chunk.len() as u64 > self.max_bytes {
                return Err(ArtworkFetchError::TooLarge {
                    limit: self.max_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }

        Ok(FetchedImage { bytes, format })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_are_fetched() {
        assert!(checked_url("https://image.tmdb.org/t/p/w500/abc.jpg").is_ok());

        for refused in [
            "http://image.tmdb.org/t/p/w500/abc.jpg",
            "ftp://example.invalid/abc.jpg",
            "file:///etc/passwd",
            "not a url",
            "",
        ] {
            assert!(
                matches!(checked_url(refused), Err(ArtworkFetchError::Refused { .. })),
                "expected {refused} to be refused",
            );
        }
    }

    #[test]
    fn a_missing_upstream_image_is_distinguished_from_a_broken_provider() {
        for gone in [404, 410] {
            assert_eq!(
                classify(gone, Some("image/jpeg"), None, 1024),
                Err(ArtworkFetchError::NotFound),
            );
        }
        assert_eq!(
            classify(503, Some("image/jpeg"), None, 1024),
            Err(ArtworkFetchError::Upstream { status: 503 }),
        );
    }

    /// The case that actually happens: a provider answers a dead image path
    /// with an HTML error page under a 200. Caching that as a poster would
    /// serve markup to an `<img>` on every later request.
    #[test]
    fn a_success_that_is_not_an_image_is_refused() {
        assert_eq!(
            classify(200, Some("text/html; charset=utf-8"), None, 1024),
            Err(ArtworkFetchError::Unsupported {
                content_type: "text/html; charset=utf-8".to_string(),
            }),
        );
        assert_eq!(
            classify(200, None, None, 1024),
            Err(ArtworkFetchError::Unsupported {
                content_type: String::new(),
            }),
        );
    }

    #[test]
    fn a_declared_length_over_the_ceiling_is_refused_before_the_body() {
        assert_eq!(
            classify(200, Some("image/png"), Some(1025), 1024),
            Err(ArtworkFetchError::TooLarge { limit: 1024 }),
        );
        assert_eq!(
            classify(200, Some("image/png"), Some(1024), 1024),
            Ok(ImageFormat::Png)
        );
    }

    /// A provider that omits `Content-Length` must not be refused here -- the
    /// ceiling is enforced again while the body is read.
    #[test]
    fn an_absent_length_defers_to_the_body_loop() {
        assert_eq!(
            classify(200, Some("image/webp"), None, 1024),
            Ok(ImageFormat::WebP)
        );
    }

    /// Derived from `ImageFormat::ALL` rather than compared to a literal, so
    /// adding a format Beam stores but does not ask for is a failure.
    #[test]
    fn every_stored_format_is_offered_to_the_provider() {
        let accept = accept_header();
        for format in ImageFormat::ALL {
            assert!(
                accept.contains(format.content_type()),
                "{} missing from Accept: {accept}",
                format.content_type(),
            );
        }
    }

    /// NFR-502: no Beam session cookie, auth header or other first-party
    /// credential may reach TMDB or AniList. Asserted on the request Beam
    /// would actually send, which needs no server to inspect.
    #[test]
    fn no_first_party_credential_is_sent_to_a_provider() {
        let client = Client::new();
        let url = checked_url("https://image.tmdb.org/t/p/w500/abc.jpg").expect("https url");
        let request = artwork_request(&client, url).expect("request builds");

        assert!(request.headers().get(reqwest::header::COOKIE).is_none());
        assert!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .is_none()
        );

        let names: Vec<_> = request
            .headers()
            .keys()
            .map(|name| name.as_str().to_ascii_lowercase())
            .collect();
        assert_eq!(
            names,
            vec!["accept".to_string()],
            "unexpected outbound headers"
        );
    }
}
