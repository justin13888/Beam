//! Fetching poster/backdrop art from the provider CDN that enrichment named.
//!
//! Beam serves artwork itself rather than handing clients a provider CDN link
//! (ADR-0015). This is the outbound half of that: one call that turns a URL
//! already stored on a title into bytes, with no cameo (or any other external
//! SDK) type in sight -- the reqwest-backed adapter lives in beam-index.
//!
//! Only URLs that enrichment itself wrote into Beam's database ever reach an
//! implementation. No client-supplied URL is ever fetched, which is what keeps
//! the proxy free of an SSRF surface without an allowlist to maintain.

use thiserror::Error;

/// An image format Beam is willing to store and serve.
///
/// An enum rather than a `String` content type on purpose: it makes "only
/// images" a property of the type instead of a check someone can forget, and
/// it is what lets the cache round-trip a content type through a filename
/// extension without a sidecar file or a metadata table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
    Avif,
}

impl ImageFormat {
    /// Every format Beam serves. The outbound `Accept` header is built from
    /// this, so adding a variant reaches the provider without a second edit.
    pub const ALL: &'static [Self] = &[Self::Jpeg, Self::Png, Self::WebP, Self::Gif, Self::Avif];

    /// Parses an HTTP `Content-Type`, ignoring any parameters after `;`.
    /// `None` for anything Beam does not serve, which is how a provider
    /// handing back an HTML error page is rejected rather than cached.
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match essence.as_str() {
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::WebP),
            "image/gif" => Some(Self::Gif),
            "image/avif" => Some(Self::Avif),
            _ => None,
        }
    }

    /// The `Content-Type` to serve this format under.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Avif => "image/avif",
        }
    }

    /// The cache filename extension. Paired with [`Self::from_extension`].
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Avif => "avif",
        }
    }

    /// Recovers the format a cached file was stored under.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::WebP),
            "gif" => Some(Self::Gif),
            "avif" => Some(Self::Avif),
            _ => None,
        }
    }
}

/// One image, fetched.
///
/// `Vec<u8>` rather than `bytes::Bytes` keeps a transport crate out of the
/// domain; the server converts once, without copying, at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
}

/// Why an artwork fetch did not produce an image.
///
/// `Clone` so a fake can replay a configured failure to every caller, and so
/// one upstream failure can be handed to every waiter that joined a
/// single-flight fetch.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArtworkFetchError {
    /// The provider no longer has this image. Distinct from the other
    /// variants because it is the one worth remembering for a while: a
    /// reorganised CDN path will not heal before the next enrichment pass.
    #[error("upstream has no such image")]
    NotFound,
    /// A response arrived, but not one Beam serves (an HTML error page, a
    /// format outside [`ImageFormat`]).
    #[error("unsupported content type: {content_type}")]
    Unsupported { content_type: String },
    /// The body exceeded the configured ceiling. Carries the ceiling, not the
    /// body size, because the body is refused before it is fully read.
    #[error("image exceeds the {limit} byte ceiling")]
    TooLarge { limit: u64 },
    /// The stored URL is not one Beam is willing to fetch (not `https`,
    /// unparseable).
    #[error("refusing to fetch {url}: {reason}")]
    Refused { url: String, reason: String },
    /// The provider answered, unhappily.
    #[error("upstream returned status {status}")]
    Upstream { status: u16 },
    /// Nothing usable came back.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Fetches one provider-hosted image.
///
/// The only network boundary the artwork path has, and therefore the only
/// thing tests substitute: the cache underneath is exercised against a real
/// `TempDir` rather than a filesystem fake. Implementations must never panic
/// on a network failure, and must never attach a Beam session cookie, header
/// or any other first-party credential to the outbound request (NFR-502).
#[async_trait::async_trait]
pub trait ArtworkFetcher: Send + Sync + std::fmt::Debug {
    async fn fetch(&self, url: &str) -> Result<FetchedImage, ArtworkFetchError>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::{Semaphore, watch};

    /// Network-free fake driving every artwork test.
    ///
    /// Two things beyond canned responses, both of which exist to make
    /// single-flight provable rather than assumed: a call counter, and a gate
    /// that holds every fetch open until the test releases it. Without the
    /// gate a fake is too fast to distinguish one upstream fetch from four.
    #[derive(Debug, Default)]
    pub struct InMemoryArtworkFetcher {
        responses: Mutex<HashMap<String, Result<FetchedImage, ArtworkFetchError>>>,
        calls: Mutex<Vec<String>>,
        gate: Mutex<Option<Arc<Gate>>>,
    }

    /// Holds fetches open, and tells the test when they have arrived.
    ///
    /// Both halves are edge-safe rather than notification-based: `watch`
    /// compares the current value before waiting, and a closed `Semaphore`
    /// fails every acquire from then on, so neither can lose a wakeup that
    /// fires between a test's check and its await.
    #[derive(Debug)]
    pub struct Gate {
        entered: watch::Sender<usize>,
        release: Semaphore,
    }

    impl Default for Gate {
        fn default() -> Self {
            Self {
                entered: watch::channel(0).0,
                release: Semaphore::new(0),
            }
        }
    }

    impl Gate {
        /// Resolves once at least `n` fetches are parked on the gate.
        pub async fn wait_for_fetches(&self, n: usize) {
            let mut rx = self.entered.subscribe();
            // `wait_for` tests the current value first, so a fetch that
            // arrived before this call still counts.
            let _ = rx.wait_for(|entered| *entered >= n).await;
        }

        /// How many fetches have reached the gate.
        pub fn entered(&self) -> usize {
            *self.entered.borrow()
        }

        /// Lets every parked fetch, and every later one, through.
        pub fn release(&self) {
            self.release.close();
        }
    }

    impl InMemoryArtworkFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        /// Serves `bytes` for `url`.
        pub fn with_image(self, url: &str, format: ImageFormat, bytes: &[u8]) -> Self {
            self.responses.lock().unwrap().insert(
                url.to_string(),
                Ok(FetchedImage {
                    bytes: bytes.to_vec(),
                    format,
                }),
            );
            self
        }

        /// Fails `url` with `error`, every time.
        pub fn with_error(self, url: &str, error: ArtworkFetchError) -> Self {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_string(), Err(error));
            self
        }

        /// Holds every fetch until the returned gate is released.
        pub fn with_gate(self) -> (Self, Arc<Gate>) {
            let gate = Arc::new(Gate::default());
            *self.gate.lock().unwrap() = Some(Arc::clone(&gate));
            (self, gate)
        }

        /// How many times the network was actually reached.
        pub fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        /// How many times `url` specifically was fetched.
        pub fn call_count_for(&self, url: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|fetched| *fetched == url)
                .count()
        }

        /// Every URL fetched, in order.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ArtworkFetcher for InMemoryArtworkFetcher {
        async fn fetch(&self, url: &str) -> Result<FetchedImage, ArtworkFetchError> {
            self.calls.lock().unwrap().push(url.to_string());

            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.entered.send_modify(|entered| *entered += 1);
                // `Err` is the open gate: `release` closes the semaphore.
                let _ = gate.release.acquire().await;
            }

            self.responses
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .unwrap_or(Err(ArtworkFetchError::NotFound))
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use test_utils::InMemoryArtworkFetcher;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The two conversions are inverses. Derived from the source rather than
    /// restating it: a variant whose extension does not parse back -- the
    /// realistic typo -- makes the cache unable to recover the content type of
    /// a file it wrote itself.
    #[test]
    fn every_format_round_trips_through_content_type_and_extension() {
        for &format in ImageFormat::ALL {
            assert_eq!(
                ImageFormat::from_content_type(format.content_type()),
                Some(format),
            );
            assert_eq!(
                ImageFormat::from_extension(format.extension()),
                Some(format)
            );
        }
    }

    #[test]
    fn content_type_parsing_ignores_parameters_and_case() {
        assert_eq!(
            ImageFormat::from_content_type("Image/JPEG; charset=binary"),
            Some(ImageFormat::Jpeg),
        );
        assert_eq!(
            ImageFormat::from_content_type("  image/webp  "),
            Some(ImageFormat::WebP),
        );
    }

    /// A provider that answers a missing image with an HTML error page must
    /// not have that page cached and served as artwork.
    #[test]
    fn non_image_content_types_are_rejected() {
        for content_type in ["text/html", "application/json", "", "image/svg+xml"] {
            assert_eq!(ImageFormat::from_content_type(content_type), None);
        }
    }

    proptest! {
        #[test]
        fn content_type_parsing_never_panics(raw in ".*") {
            let _ = ImageFormat::from_content_type(&raw);
        }

        #[test]
        fn extension_parsing_never_panics(raw in ".*") {
            let _ = ImageFormat::from_extension(&raw);
        }
    }
}
