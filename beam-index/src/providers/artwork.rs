//! `reqwest`-backed [`ArtworkFetcher`] adapter.
//!
//! Lives here rather than in beam-server for the same reason the cameo
//! adapter does: this crate already owns every outbound provider call, and
//! beam-server has no HTTP client of its own to grow.
//!
//! The transport glue is deliberately thin, and the decisions it makes are
//! pulled out into [`checked_url`], [`classify`] and [`artwork_request`] --
//! pure functions with no socket in them, which is what lets the policy be
//! tested exhaustively without a TLS listener. What only a connection can
//! show -- the per-chunk body ceiling and the redirect policy -- is tested
//! against a local TLS stand-in on 127.0.0.1, reached through
//! [`ReqwestArtworkFetcher::with_root_certificate`]. Everything above this
//! adapter substitutes `InMemoryArtworkFetcher` instead.

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
///
/// Load-bearing for the cache, and not obviously so: TMDB content-negotiates
/// on this, returning WebP for a `.jpg` path when WebP is offered. A provider
/// URL therefore names one representation *per `Accept`*, not one absolutely.
/// The cache stays correct because this header is constant -- the same URL
/// always yields the same bytes for Beam -- so keep it derived from
/// `ImageFormat::ALL` rather than varying it per request. See ADR-0015.
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
        Self::build(limits, Vec::new())
    }

    /// As [`Self::new`], but also trusting `root` alongside the platform's
    /// roots -- for a provider mirror behind a private CA, and the seam the
    /// TLS stand-in tests use to reach a listener with a self-signed
    /// certificate. Nothing else about the client differs, so a test through
    /// here exercises the same redirect policy and body ceiling as production.
    pub fn with_root_certificate(
        limits: ArtworkFetchLimits,
        root: reqwest::Certificate,
    ) -> Result<Self, reqwest::Error> {
        Self::build(limits, vec![root])
    }

    fn build(
        limits: ArtworkFetchLimits,
        extra_roots: Vec<reqwest::Certificate>,
    ) -> Result<Self, reqwest::Error> {
        let redirect = reqwest::redirect::Policy::custom(|attempt| {
            // Named rather than chained, because the two reasons to refuse are
            // different -- one is a redirect loop, the other a downgrade to
            // cleartext -- while the answer to both is the same.
            //
            // `previous()` holds every URL already requested, the original one
            // included, so it is one longer than the redirects followed so
            // far: `>` lets exactly `MAX_REDIRECTS` through, as reqwest's own
            // `Policy::limited` does.
            let looping = attempt.previous().len() > MAX_REDIRECTS;
            let downgraded = attempt.url().scheme() != "https";
            if looping || downgraded {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });

        let client = Client::builder()
            .timeout(limits.timeout)
            .redirect(redirect)
            .user_agent(concat!("Beam/", env!("CARGO_PKG_VERSION")))
            .tls_certs_merge(extra_roots)
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
    use std::sync::Arc;

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

    /// A provider nobody answers at is a transport failure -- which the
    /// artwork service retries -- not a missing image it would remember.
    #[tokio::test]
    async fn an_unreachable_provider_is_a_transport_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = listener.local_addr().expect("its address");
        drop(listener);
        let fetcher = ReqwestArtworkFetcher::new(ArtworkFetchLimits {
            timeout: Duration::from_secs(5),
            max_bytes: 1024,
        })
        .expect("the client builds");

        let fetched = fetcher
            .fetch(&format!("https://{address}/poster.jpg"))
            .await;
        assert!(
            matches!(fetched, Err(ArtworkFetchError::Transport(_))),
            "{fetched:?}"
        );
        // A cleartext URL is refused before anything is sent.
        assert!(matches!(
            fetcher.fetch(&format!("http://{address}/poster.jpg")).await,
            Err(ArtworkFetchError::Refused { .. })
        ));
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

    const CEILING: u64 = 1024;

    fn trusting(root: reqwest::Certificate) -> ReqwestArtworkFetcher {
        ReqwestArtworkFetcher::with_root_certificate(
            ArtworkFetchLimits {
                timeout: Duration::from_secs(5),
                max_bytes: CEILING,
            },
            root,
        )
        .expect("the client builds")
    }

    /// A body sent without `Content-Length` gets past `classify`, so only the
    /// per-chunk check in `fetch` stands between it and an unbounded buffer.
    /// The body exactly at the ceiling is the control: it must still arrive.
    #[tokio::test]
    async fn an_undeclared_body_over_the_ceiling_is_refused_while_it_is_read() {
        let half = usize::try_from(CEILING / 2).expect("fits");
        let (server, root) = stand_in::https(Arc::new(move |path: &str| match path {
            "/over.png" => stand_in::chunked_png(&[half, half + 1]),
            "/exact.png" => stand_in::chunked_png(&[half, half]),
            _ => stand_in::not_found(),
        }))
        .await;
        let fetcher = trusting(root);

        assert_eq!(
            fetcher.fetch(&server.url("https", "/over.png")).await,
            Err(ArtworkFetchError::TooLarge { limit: CEILING }),
        );

        let exact = fetcher
            .fetch(&server.url("https", "/exact.png"))
            .await
            .expect("a body at the ceiling is accepted");
        assert_eq!(exact.format, ImageFormat::Png);
        assert_eq!(exact.bytes.len() as u64, CEILING);
    }

    /// A redirect off `https` is not followed: the redirect itself comes back
    /// as the provider's answer, and the cleartext stand-in -- which would
    /// have served a perfectly good image -- never sees a request.
    #[tokio::test]
    async fn a_redirect_down_to_cleartext_is_not_followed() {
        let cleartext = stand_in::http(Arc::new(|_: &str| stand_in::png_with_length(16))).await;
        let target = cleartext.url("http", "/poster.png");
        let (server, root) = stand_in::https(Arc::new(move |path: &str| match path {
            "/downgrade.png" => stand_in::redirect(&target),
            _ => stand_in::not_found(),
        }))
        .await;

        assert_eq!(
            trusting(root)
                .fetch(&server.url("https", "/downgrade.png"))
                .await,
            Err(ArtworkFetchError::Upstream { status: 302 }),
        );
        assert_eq!(server.requested(), vec!["/downgrade.png".to_string()]);
        assert!(
            cleartext.requested().is_empty(),
            "{:?}",
            cleartext.requested()
        );
    }

    /// `MAX_REDIRECTS` counts redirects, not requests: a chain of exactly that
    /// many arrives, and one more stops at the redirect it refused -- whose
    /// target is never requested.
    #[tokio::test]
    async fn a_redirect_chain_is_followed_up_to_the_cap_and_no_further() {
        let (server, root) = stand_in::https(Arc::new(|path: &str| {
            match path
                .strip_prefix("/chain/")
                .and_then(|hops| hops.parse::<usize>().ok())
            {
                Some(0) => stand_in::png_with_length(16),
                Some(hops) => stand_in::redirect(&format!("/chain/{}", hops - 1)),
                None => stand_in::not_found(),
            }
        }))
        .await;
        let fetcher = trusting(root);

        let over = MAX_REDIRECTS + 1;
        assert_eq!(
            fetcher
                .fetch(&server.url("https", &format!("/chain/{over}")))
                .await,
            Err(ArtworkFetchError::Upstream { status: 302 }),
        );
        let followed: Vec<String> = (1..=over)
            .rev()
            .map(|hops| format!("/chain/{hops}"))
            .collect();
        assert_eq!(server.requested(), followed);

        let at_cap = fetcher
            .fetch(&server.url("https", &format!("/chain/{MAX_REDIRECTS}")))
            .await
            .expect("a chain of exactly MAX_REDIRECTS is followed");
        assert_eq!(at_cap.bytes.len(), 16);
    }

    /// A TLS listener on 127.0.0.1 with a fresh self-signed certificate, and a
    /// plain one for the cleartext end of a downgrade. Each answers one
    /// request per connection with whatever bytes its route returns for the
    /// path, and records the paths it was asked for.
    mod stand_in {
        use std::net::SocketAddr;
        use std::sync::Arc;

        use parking_lot::Mutex;
        use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio_rustls::TlsAcceptor;
        use tokio_rustls::rustls::ServerConfig;
        use tokio_rustls::rustls::crypto::aws_lc_rs;
        use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

        pub(super) type Route = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

        pub(super) struct StandIn {
            address: SocketAddr,
            requested: Arc<Mutex<Vec<String>>>,
        }

        impl StandIn {
            pub(super) fn url(&self, scheme: &str, path: &str) -> String {
                format!("{scheme}://{}{path}", self.address)
            }

            pub(super) fn requested(&self) -> Vec<String> {
                self.requested.lock().clone()
            }
        }

        pub(super) async fn https(route: Route) -> (StandIn, reqwest::Certificate) {
            // An IP SAN, so the URL names 127.0.0.1 directly and nothing
            // depends on how `localhost` resolves on the host.
            let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
                .expect("a self-signed certificate");
            let certificate = certified.cert.der().clone();
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                certified.signing_key.serialize_der(),
            ));
            // An explicit provider: more than one is compiled into rustls in
            // this workspace, so there is no process default to fall back on.
            let config =
                ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                    .with_safe_default_protocol_versions()
                    .expect("the default protocol versions")
                    .with_no_client_auth()
                    .with_single_cert(vec![certificate.clone()], key)
                    .expect("the server config");
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let root = reqwest::Certificate::from_der(&certificate).expect("a DER certificate");

            let (listener, stand_in) = bind().await;
            let requested = Arc::clone(&stand_in.requested);
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (acceptor, route, requested) =
                        (acceptor.clone(), Arc::clone(&route), Arc::clone(&requested));
                    tokio::spawn(async move {
                        if let Ok(tls) = acceptor.accept(tcp).await {
                            serve(tls, &route, &requested).await;
                        }
                    });
                }
            });
            (stand_in, root)
        }

        pub(super) async fn http(route: Route) -> StandIn {
            let (listener, stand_in) = bind().await;
            let requested = Arc::clone(&stand_in.requested);
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (route, requested) = (Arc::clone(&route), Arc::clone(&requested));
                    tokio::spawn(async move { serve(tcp, &route, &requested).await });
                }
            });
            stand_in
        }

        async fn bind() -> (TcpListener, StandIn) {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
            let address = listener.local_addr().expect("its address");
            let stand_in = StandIn {
                address,
                requested: Arc::default(),
            };
            (listener, stand_in)
        }

        async fn serve<S>(mut stream: S, route: &Route, requested: &Mutex<Vec<String>>)
        where
            S: AsyncRead + AsyncWrite + Unpin,
        {
            let mut head = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => head.extend_from_slice(&buffer[..read]),
                }
            }
            let path = String::from_utf8_lossy(&head)
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let response = route(&path);
            requested.lock().push(path);
            // Errors here mean the client hung up, which is its business.
            let _ = stream.write_all(&response).await;
            let _ = stream.shutdown().await;
        }

        /// A PNG body sent in the given chunk sizes, with no `Content-Length`.
        pub(super) fn chunked_png(chunks: &[usize]) -> Vec<u8> {
            let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                .to_vec();
            for &size in chunks {
                response.extend_from_slice(format!("{size:x}\r\n").as_bytes());
                response.extend(std::iter::repeat_n(b'p', size));
                response.extend_from_slice(b"\r\n");
            }
            response.extend_from_slice(b"0\r\n\r\n");
            response
        }

        pub(super) fn png_with_length(size: usize) -> Vec<u8> {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
                 Content-Length: {size}\r\nConnection: close\r\n\r\n"
            )
            .into_bytes();
            response.extend(std::iter::repeat_n(b'p', size));
            response
        }

        pub(super) fn redirect(location: &str) -> Vec<u8> {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        }

        pub(super) fn not_found() -> Vec<u8> {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    }
}
