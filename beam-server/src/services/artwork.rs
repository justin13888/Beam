//! The artwork cache: one upstream fetch per image, however many clients ask.
//!
//! Beam serves poster and backdrop art itself (ADR-0015), so every viewer
//! browsing a library hits this rather than a provider CDN. That makes three
//! properties load-bearing, and each is why a piece of this exists:
//!
//! * **Single-flight.** Twenty viewers opening the same grid produce one
//!   upstream request per image, not twenty. Requests for a cold key serialise
//!   on a per-key lane and every one but the first finds the entry already on
//!   disk.
//! * **No invalidation.** The cache key is the upstream URL, and TMDB and
//!   AniList both serve content-addressed immutable image paths, so bytes
//!   never change under a URL. The only turnover event is enrichment writing a
//!   *different* URL to a title, which is a different key, a different ETag
//!   and fresh bytes -- and strands the old entry for the LRU to reclaim. High
//!   turnover costs one extra cache entry, never a stale poster. (This is the
//!   one assumption the design rests on; it is recorded in ADR-0015 so that it
//!   can be falsified. If it ever fails, the fix is a TTL on an entry, not a
//!   redesign.)
//! * **One tier.** There is no in-process byte cache above the files: the OS
//!   page cache already holds every recently served poster, and a second LRU
//!   over `Bytes` would duplicate it while needing its own cap, its own
//!   eviction and its own accounting. At poster size a warm read is a memcpy.
//!
//! A size a client asks for (issue #266) is one more entry of the same cache:
//! resized from the cached original once, keyed by the URL and the width, and
//! evicted by the same LRU under the same ceiling. Beam resizes itself rather
//! than asking for a provider's sized rendition, because that would mean
//! rewriting each provider's URL scheme -- TMDB has one, AniList has none --
//! and every fetched URL would stop being one enrichment wrote.
//!
//! The index is what makes a hit one `open` rather than a directory scan, and
//! it is rebuilt from the directory at startup, so the files on disk remain the
//! thing that survives a restart.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use beam_domain::providers::artwork::{
    ArtworkFetchError, ArtworkFetcher, FetchedImage, ImageFormat,
};
use beam_domain::services::Clock;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tracing::{debug, warn};

/// How many upstream failures are remembered at once. A ceiling rather than a
/// policy: the TTL is what expires an entry, this only stops a provider
/// failing across a whole library from growing the map without bound.
const MAX_NEGATIVE_ENTRIES: usize = 4096;

/// How many sizes are remembered as served at the original's size. Bounded
/// for the same reason as [`MAX_NEGATIVE_ENTRIES`]; forgetting one only costs
/// one more decode.
const MAX_PASSTHROUGH_ENTRIES: usize = 4096;

/// The widest or tallest image Beam will decode to resize. A provider's
/// largest original backdrop is under 4,000 pixels wide; anything past this is
/// a decompression bomb, not a poster, and is served at its original size.
const MAX_DECODE_DIMENSION: u32 = 12_000;

/// The most memory one decode may allocate. An 8,000 by 8,000 RGBA image is
/// exactly this, so every real poster and backdrop fits with room to spare.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// The JPEG quality a resized rendition is encoded at. High enough that a
/// downscaled poster shows no blocking at the size it is drawn; the size win
/// comes from the pixel count, not from starving the encoder.
const RESIZED_JPEG_QUALITY: u8 = 85;

/// What downscaling one image came to.
#[derive(Debug)]
enum Downscale {
    /// A narrower rendition, ready to cache.
    Resized(FetchedImage),
    /// The original is already no wider than asked, so it is the rendition:
    /// Beam never upscales.
    AlreadyNarrow,
    /// Beam does not resize this image -- a format it does not decode (GIF,
    /// AVIF), or bytes that do not decode within the limits -- so it is served
    /// at its original size, which every client can still draw.
    NotResizable(String),
}

/// Scales `bytes` to `width` pixels wide, keeping the aspect ratio.
///
/// Synchronous and CPU-bound: the caller runs it on the blocking pool. The
/// rendition is a PNG where the image has transparency and a JPEG where it has
/// none, whatever the source was -- a poster has no use for lossless pixels,
/// and a logo's transparency must survive.
fn downscale(bytes: &[u8], format: ImageFormat, width: NonZeroU32) -> Downscale {
    let codec = match format {
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::WebP => image::ImageFormat::WebP,
        ImageFormat::Gif | ImageFormat::Avif => {
            return Downscale::NotResizable(format!("{} is not resized", format.extension()));
        }
    };

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);

    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), codec);
    reader.limits(limits);
    let decoded = reader.into_decoder().and_then(|mut decoder| {
        use image::ImageDecoder as _;
        // The original is drawn upright by any client that honours its
        // orientation tag, and the rendition carries no tag, so it is turned
        // upright here or it would be served on its side.
        let orientation = decoder.orientation()?;
        let mut decoded = image::DynamicImage::from_decoder(decoder)?;
        decoded.apply_orientation(orientation);
        Ok(decoded)
    });
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(err) => return Downscale::NotResizable(err.to_string()),
    };

    let (original_width, original_height) = (decoded.width(), decoded.height());
    let width = width.get();
    if original_width <= width {
        return Downscale::AlreadyNarrow;
    }
    // Rounded to the nearest pixel, and never zero: a 4,000 by 1 strip scaled
    // to 185 wide is still one pixel tall.
    let height = (u64::from(original_height) * u64::from(width) + u64::from(original_width) / 2)
        / u64::from(original_width);
    let height = u32::try_from(height.max(1)).expect("narrower than the original, so it fits");
    let resized = decoded.resize_exact(width, height, image::imageops::FilterType::Lanczos3);

    let mut encoded = Vec::new();
    let written = if resized.color().has_alpha() {
        resized
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .map(|()| ImageFormat::Png)
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, RESIZED_JPEG_QUALITY)
            .encode_image(&resized.to_rgb8())
            .map(|()| ImageFormat::Jpeg)
    };
    match written {
        Ok(format) => Downscale::Resized(FetchedImage {
            bytes: encoded,
            format,
        }),
        Err(err) => Downscale::NotResizable(err.to_string()),
    }
}

/// The resize the cache runs on the blocking pool: [`downscale`] in the
/// process, and a test's wrapper around it where a test has to pause a decode
/// at a known point -- which is the only way to observe what a request
/// dropped mid-decode still holds.
#[derive(Clone)]
struct Downscaler(Arc<DownscaleFn>);

/// The shape of [`downscale`].
type DownscaleFn = dyn Fn(&[u8], ImageFormat, NonZeroU32) -> Downscale + Send + Sync;

impl std::fmt::Debug for Downscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Downscaler")
    }
}

/// The identity of one cached image: a digest of the URL it came from.
///
/// Hex, and half a SHA-256 -- 128 bits is far past collision relevance for a
/// library's worth of artwork, and it keeps the filename short.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CacheKey(String);

impl CacheKey {
    /// The key for an upstream artwork URL.
    pub fn for_url(url: &str) -> Self {
        let digest = Sha256::digest(url.as_bytes());
        Self(beam_auth::utils::hex::encode_lower(&digest[..16]))
    }

    /// The key for `url` scaled to `width` pixels wide (issue #266).
    ///
    /// A digest like [`Self::for_url`]'s, so every size is its own file, its
    /// own LRU entry and its own strong `ETag`, and it moves with the URL
    /// exactly as the original's does. The NUL separator cannot occur in a
    /// URL, so no sized key is ever the key of some other URL's original.
    pub fn for_sized(url: &str, width: NonZeroU32) -> Self {
        Self::for_url(&format!("{url}\0w{width}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One image, ready to serve.
#[derive(Debug, Clone)]
pub struct CachedImage {
    pub bytes: Bytes,
    pub format: ImageFormat,
    /// The cache key, which is also the entity-independent `ETag` the endpoint
    /// serves. It changes exactly when enrichment points a title at different
    /// artwork, which is what makes revalidation correct without a TTL.
    pub key: CacheKey,
}

/// What the cache knows about one file without opening it.
#[derive(Debug, Clone, Copy)]
struct Entry {
    format: ImageFormat,
    size: u64,
    /// Access counter rather than a timestamp: LRU only needs an order, and a
    /// counter cannot tie, so eviction is deterministic under test.
    last_used: u64,
}

#[derive(Debug, Default)]
struct Index {
    entries: HashMap<CacheKey, Entry>,
    total_bytes: u64,
    ticks: u64,
}

impl Index {
    fn touch(&mut self, key: &CacheKey) -> Option<Entry> {
        self.ticks += 1;
        let tick = self.ticks;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = tick;
        Some(*entry)
    }

    /// Indexes an entry, returning the one it displaced.
    ///
    /// The caller needs the whole displaced entry rather than only its size,
    /// because one displaced under a *different* format names a different
    /// file: [`ArtworkCache::path_for`] derives one path per key from the
    /// format the index holds, so the moment the new format is indexed the old
    /// file is unreachable -- never read, never chosen by eviction, and
    /// counted by nothing. Only the caller has the path, so only the caller
    /// can delete it.
    fn insert(&mut self, key: CacheKey, format: ImageFormat, size: u64) -> Option<Entry> {
        self.ticks += 1;
        let displaced = self.entries.insert(
            key,
            Entry {
                format,
                size,
                last_used: self.ticks,
            },
        );
        if let Some(displaced) = displaced {
            self.total_bytes = self.total_bytes.saturating_sub(displaced.size);
        }
        self.total_bytes = self.total_bytes.saturating_add(size);
        displaced
    }

    /// The key that should go next when the cache is over its ceiling.
    fn least_recently_used(&self) -> Option<CacheKey> {
        self.entries
            .iter()
            .min_by_key(|(key, entry)| (entry.last_used, (*key).clone()))
            .map(|(key, _)| key.clone())
    }

    fn remove(&mut self, key: &CacheKey) -> Option<Entry> {
        let entry = self.entries.remove(key)?;
        self.total_bytes = self.total_bytes.saturating_sub(entry.size);
        Some(entry)
    }
}

/// How the cache is sized and how long it sulks after a failure.
#[derive(Debug, Clone)]
pub struct ArtworkCacheConfig {
    /// Where the files live. One directory, sharded one level by key prefix.
    pub root: PathBuf,
    /// The ceiling the LRU evicts down to.
    pub max_bytes: u64,
    /// How long an upstream failure is remembered, so a provider that is down
    /// (or a title whose art has been deleted) is not re-fetched once per
    /// client per grid render.
    pub negative_ttl: Duration,
}

/// Content-keyed artwork cache over one directory.
#[derive(Debug)]
pub struct ArtworkCache {
    config: ArtworkCacheConfig,
    fetcher: Arc<dyn ArtworkFetcher>,
    clock: Arc<dyn Clock>,
    index: Mutex<Index>,
    negative: Mutex<HashMap<CacheKey, (Instant, ArtworkFetchError)>>,
    /// One lane per in-flight key. `Weak` so a lane disappears once the last
    /// request holding it is done, rather than the map growing per key ever
    /// requested.
    lanes: Mutex<HashMap<CacheKey, std::sync::Weak<AsyncMutex<()>>>>,
    /// Sized keys whose rendition is the original itself -- already narrow
    /// enough, or not resizable -- so the decode that found that out is not
    /// repeated per request. No TTL: the bytes under a URL never change.
    passthrough: Mutex<HashSet<CacheKey>>,
    /// Bounds how many resizes run at once. Each holds a decoded poster in
    /// memory and a blocking thread busy, and a cold grid asks for dozens of
    /// different posters at the same moment. `Arc` so a permit can be owned by
    /// the blocking decode itself rather than by the request awaiting it.
    resizes: Arc<Semaphore>,
    downscaler: Downscaler,
}

impl ArtworkCache {
    /// An empty cache over `config.root`, touching no disk.
    ///
    /// Separate from [`Self::restore`] because it is infallible and
    /// synchronous, which is what lets a caller that has no runtime yet --
    /// a test fixture, notably -- have one at all. A cache built this way is
    /// correct but cold: it serves nothing until it has fetched it.
    pub fn new(
        config: ArtworkCacheConfig,
        fetcher: Arc<dyn ArtworkFetcher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config,
            fetcher,
            clock,
            index: Mutex::new(Index::default()),
            negative: Mutex::new(HashMap::new()),
            lanes: Mutex::new(HashMap::new()),
            passthrough: Mutex::new(HashSet::new()),
            resizes: Arc::new(Semaphore::new(
                std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
            )),
            downscaler: Downscaler(Arc::new(downscale)),
        }
    }

    /// Replaces the resize with `downscaler`, so a test can hold a decode
    /// open while it inspects the cache.
    #[cfg(test)]
    fn with_downscaler(
        mut self,
        downscaler: impl Fn(&[u8], ImageFormat, NonZeroU32) -> Downscale + Send + Sync + 'static,
    ) -> Self {
        self.downscaler = Downscaler(Arc::new(downscaler));
        self
    }

    /// Rebuilds the index from the files already in the cache directory.
    ///
    /// A cache that survives a restart is most of the point: a cold start
    /// after a deploy should not re-fetch a whole library's art from TMDB.
    pub async fn restore(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.config.root).await?;
        let restored = Self::scan(&self.config.root).await?;
        *self.index.lock().expect("artwork index poisoned") = restored;
        Ok(())
    }

    /// [`Self::new`] followed by [`Self::restore`] -- what the process does at
    /// startup.
    pub async fn open(
        config: ArtworkCacheConfig,
        fetcher: Arc<dyn ArtworkFetcher>,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        let cache = Self::new(config, fetcher, clock);
        cache.restore().await?;
        Ok(cache)
    }

    /// Rebuilds the index by walking the shard directories.
    ///
    /// The filename carries both halves of what an entry needs -- the key and,
    /// through its extension, the format -- so nothing beside the files has to
    /// be persisted, and a half-written or foreign file is simply not indexed.
    async fn scan(root: &Path) -> std::io::Result<Index> {
        let mut index = Index::default();
        let mut shards = tokio::fs::read_dir(root).await?;

        while let Some(shard) = shards.next_entry().await? {
            if !shard.file_type().await?.is_dir() {
                continue;
            }
            let mut files = tokio::fs::read_dir(shard.path()).await?;
            while let Some(file) = files.next_entry().await? {
                let path = file.path();
                let (Some(stem), Some(extension)) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    path.extension().and_then(|s| s.to_str()),
                ) else {
                    continue;
                };
                let Some(format) = ImageFormat::from_extension(extension) else {
                    continue;
                };
                let Ok(metadata) = file.metadata().await else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let displaced = index.insert(CacheKey(stem.to_string()), format, metadata.len());

                // Two formats for one key, which a directory can hold if an
                // eviction failed to unlink a file and the provider later
                // answered the same URL with a different format. Only one is
                // reachable -- the index holds one format per key -- so the
                // other would sit here unread and unevictable, keeping the
                // cache permanently above its ceiling. Whichever the directory
                // yielded first loses; both are valid images of the same URL,
                // so it does not matter which survives.
                if let Some(displaced) = displaced
                    && displaced.format != format
                {
                    let stale = path.with_extension(displaced.format.extension());
                    Self::remove_stale_file(&stale).await;
                }
            }
        }

        debug!(
            entries = index.entries.len(),
            bytes = index.total_bytes,
            "artwork cache index rebuilt"
        );
        Ok(index)
    }

    fn path_for(&self, key: &CacheKey, format: ImageFormat) -> PathBuf {
        let shard: String = key.as_str().chars().take(2).collect();
        self.config
            .root
            .join(shard)
            .join(format!("{}.{}", key.as_str(), format.extension()))
    }

    /// The image for `url`, from disk if it is there and from the provider if
    /// it is not.
    pub async fn get(&self, url: &str) -> Result<CachedImage, ArtworkFetchError> {
        let key = CacheKey::for_url(url);

        if let Some(remembered) = self.remembered_failure(&key) {
            return Err(remembered);
        }
        if let Some(hit) = self.read(&key).await {
            return Ok(hit);
        }

        // Everything past here is the cold path, and only one request per key
        // walks it at a time.
        let lane = self.lane(&key);
        let _held = lane.lock().await;

        // The request that held the lane before this one may have filled the
        // entry, or learned that it cannot be filled.
        if let Some(remembered) = self.remembered_failure(&key) {
            return Err(remembered);
        }
        if let Some(hit) = self.read(&key).await {
            return Ok(hit);
        }

        match self.fetcher.fetch(url).await {
            Ok(image) => {
                let cached = self.store(&key, image).await;
                Ok(cached)
            }
            Err(err) => {
                self.remember_failure(&key, &err);
                Err(err)
            }
        }
    }

    /// The image for `url` scaled to `width` pixels wide (issue #266), resized
    /// once and cached beside the original under its own key.
    ///
    /// Where the original is no wider than `width`, or is not an image Beam
    /// resizes, the original is the answer -- under the original's key, since
    /// its bytes are the original's. A failure to fetch the original is the
    /// only error: a size that cannot be made degrades to the full-size image
    /// rather than to a broken one.
    pub async fn get_sized(
        &self,
        url: &str,
        width: NonZeroU32,
    ) -> Result<CachedImage, ArtworkFetchError> {
        let key = CacheKey::for_sized(url, width);

        if self.is_passthrough(&key) {
            return self.get(url).await;
        }
        if let Some(hit) = self.read(&key).await {
            return Ok(hit);
        }

        // One resize per key at a time, as one fetch per key is: a grid of
        // viewers asking for the same cold size decode it once.
        let held = self.lane(&key).lock_owned().await;

        if self.is_passthrough(&key) {
            return self.get(url).await;
        }
        if let Some(hit) = self.read(&key).await {
            return Ok(hit);
        }

        let original = self.get(url).await?;
        let permit = Arc::clone(&self.resizes)
            .acquire_owned()
            .await
            .expect("the resize semaphore is never closed");
        let bytes = original.bytes.clone();
        let format = original.format;
        let downscaler = self.downscaler.clone();
        // The permit and the lane guard move into the blocking task, because a
        // spawned blocking task runs to completion even when the request
        // awaiting it is dropped -- a client disconnect. Held by the request,
        // both would be released at that moment while the decode carried on,
        // letting cancelled requests exceed the concurrency bound and a retry
        // start a second decode of the same key. The permit is released as the
        // decode ends; the guard comes back out so the lane stays held through
        // the store below, and is dropped with the task's output if nobody is
        // left to receive it.
        let outcome = tokio::task::spawn_blocking(move || {
            let downscaled = (downscaler.0)(&bytes, format, width);
            drop(permit);
            (downscaled, held)
        })
        .await;
        // On a panic the guard was dropped while unwinding, which releases the
        // lane; a Tokio mutex is not poisoned by it.
        let (outcome, _held) = match outcome {
            Ok((downscaled, held)) => (Ok(downscaled), Some(held)),
            Err(err) => (Err(err), None),
        };

        match outcome {
            Ok(Downscale::Resized(rendition)) => Ok(self.store(&key, rendition).await),
            Ok(Downscale::AlreadyNarrow) => {
                self.remember_passthrough(&key);
                Ok(original)
            }
            Ok(Downscale::NotResizable(reason)) => {
                debug!(%url, %width, %reason, "artwork served at its original size");
                self.remember_passthrough(&key);
                Ok(original)
            }
            // A decoder that panicked on these bytes will panic on them again,
            // so this is remembered like any other image Beam cannot resize.
            Err(err) => {
                warn!(%url, %width, %err, "artwork resize did not finish");
                self.remember_passthrough(&key);
                Ok(original)
            }
        }
    }

    fn is_passthrough(&self, key: &CacheKey) -> bool {
        self.passthrough
            .lock()
            .expect("artwork passthrough set poisoned")
            .contains(key)
    }

    fn remember_passthrough(&self, key: &CacheKey) {
        let mut passthrough = self
            .passthrough
            .lock()
            .expect("artwork passthrough set poisoned");
        // Forgetting is cheap -- one more decode -- so a full set is simply
        // started over rather than ordered for eviction.
        if passthrough.len() >= MAX_PASSTHROUGH_ENTRIES {
            passthrough.clear();
        }
        passthrough.insert(key.clone());
    }

    /// The lane for `key`, shared with every other request for it.
    fn lane(&self, key: &CacheKey) -> Arc<AsyncMutex<()>> {
        let mut lanes = self.lanes.lock().expect("artwork lane map poisoned");
        if let Some(existing) = lanes.get(key).and_then(std::sync::Weak::upgrade) {
            return existing;
        }
        let lane = Arc::new(AsyncMutex::new(()));
        lanes.insert(key.clone(), Arc::downgrade(&lane));
        // Bounded maintenance: drop the lanes nobody holds any more, so a
        // library's worth of one-off keys does not accumulate.
        lanes.retain(|_, held| held.strong_count() > 0);
        lane
    }

    /// Reads a cached image, or `None` if this key is not in the index.
    async fn read(&self, key: &CacheKey) -> Option<CachedImage> {
        let entry = {
            let mut index = self.index.lock().expect("artwork index poisoned");
            index.touch(key)?
        };

        let path = self.path_for(key, entry.format);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Some(CachedImage {
                bytes: Bytes::from(bytes),
                format: entry.format,
                key: key.clone(),
            }),
            Err(err) => {
                // Indexed but unreadable: something outside Beam removed it.
                // Drop the entry so the next request re-fetches rather than
                // failing forever on a file that is not coming back.
                if err.kind() != ErrorKind::NotFound {
                    warn!(?path, %err, "artwork cache entry unreadable");
                }
                self.index
                    .lock()
                    .expect("artwork index poisoned")
                    .remove(key);
                None
            }
        }
    }

    /// Writes an image to disk and indexes it, then evicts down to the
    /// ceiling.
    ///
    /// A write that fails is not an error the caller sees: the bytes are in
    /// hand and serving them is strictly better than a broken image. It only
    /// means the next request fetches again.
    async fn store(&self, key: &CacheKey, image: FetchedImage) -> CachedImage {
        let FetchedImage { bytes, format } = image;
        let path = self.path_for(key, format);

        if let Err(err) = self.write_file(&path, &bytes).await {
            warn!(?path, %err, "could not cache artwork; serving it uncached");
            return CachedImage {
                bytes: Bytes::from(bytes),
                format,
                key: key.clone(),
            };
        }

        let displaced = self.index.lock().expect("artwork index poisoned").insert(
            key.clone(),
            format,
            bytes.len() as u64,
        );

        // The same key under a different format was just written to a
        // different filename, so the file the old one named is now unreachable.
        // See `Index::insert`.
        if let Some(displaced) = displaced
            && displaced.format != format
        {
            Self::remove_stale_file(&self.path_for(key, displaced.format)).await;
        }

        self.evict_to_ceiling().await;

        CachedImage {
            bytes: Bytes::from(bytes),
            format,
            key: key.clone(),
        }
    }

    /// Writes via a temporary file and a rename, so a reader never observes a
    /// partially written image and the index never describes one.
    async fn write_file(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let staged = path.with_extension("part");
        tokio::fs::write(&staged, bytes).await?;
        tokio::fs::rename(&staged, path).await
    }

    /// Unlinks a cache file nothing can reach any more.
    ///
    /// Best-effort by design: the entry it belonged to is already out of the
    /// index, so a file that cannot be removed costs disk rather than
    /// correctness, and failing a request a viewer is waiting on to report it
    /// would be the worse trade.
    async fn remove_stale_file(path: &Path) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => debug!(?path, "removed superseded artwork cache entry"),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => warn!(?path, %err, "could not remove superseded artwork cache entry"),
        }
    }

    /// Evicts least-recently-used entries until the cache is under its
    /// ceiling.
    async fn evict_to_ceiling(&self) {
        loop {
            let victim = {
                let mut index = self.index.lock().expect("artwork index poisoned");
                if index.total_bytes <= self.config.max_bytes {
                    return;
                }
                let Some(key) = index.least_recently_used() else {
                    return;
                };
                index.remove(&key).map(|entry| (key, entry))
            };

            let Some((key, entry)) = victim else {
                return;
            };
            let path = self.path_for(&key, entry.format);
            if let Err(err) = tokio::fs::remove_file(&path).await
                && err.kind() != ErrorKind::NotFound
            {
                warn!(?path, %err, "could not evict artwork cache entry");
            }
            debug!(%key, bytes = entry.size, "evicted artwork cache entry");
        }
    }

    /// The remembered failure for `key`, if one is still within its TTL.
    fn remembered_failure(&self, key: &CacheKey) -> Option<ArtworkFetchError> {
        let now = self.clock.monotonic();
        let mut negative = self
            .negative
            .lock()
            .expect("artwork negative cache poisoned");
        let (recorded, err) = negative.get(key)?;
        if now.duration_since(*recorded) >= self.config.negative_ttl {
            negative.remove(key);
            return None;
        }
        Some(err.clone())
    }

    fn remember_failure(&self, key: &CacheKey, err: &ArtworkFetchError) {
        let now = self.clock.monotonic();
        let mut negative = self
            .negative
            .lock()
            .expect("artwork negative cache poisoned");

        if negative.len() >= MAX_NEGATIVE_ENTRIES {
            negative.retain(|_, (recorded, _)| {
                now.duration_since(*recorded) < self.config.negative_ttl
            });
        }
        if negative.len() >= MAX_NEGATIVE_ENTRIES {
            return;
        }
        negative.insert(key.clone(), (now, err.clone()));
    }
}

#[path = "artwork_tests.rs"]
mod artwork_tests;
