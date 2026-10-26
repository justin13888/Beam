#[cfg(test)]
mod tests {
    use crate::services::artwork::{ArtworkCache, ArtworkCacheConfig, CacheKey};
    use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
    use beam_domain::providers::artwork::{ArtworkFetchError, ImageFormat};
    use beam_domain::services::{Clock, TestClock};
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    const POSTER: &str = "https://image.tmdb.org/t/p/w500/poster.jpg";
    const OTHER: &str = "https://image.tmdb.org/t/p/w500/refreshed.jpg";
    const TTL: Duration = Duration::from_secs(300);

    fn config(root: &TempDir, max_bytes: u64) -> ArtworkCacheConfig {
        ArtworkCacheConfig {
            root: root.path().join("artwork"),
            max_bytes,
            negative_ttl: TTL,
        }
    }

    async fn cache(
        root: &TempDir,
        max_bytes: u64,
        fetcher: Arc<InMemoryArtworkFetcher>,
        clock: Arc<dyn Clock>,
    ) -> ArtworkCache {
        ArtworkCache::open(config(root, max_bytes), fetcher, clock)
            .await
            .expect("cache opens")
    }

    #[tokio::test]
    async fn a_second_request_is_served_from_disk_without_touching_the_provider() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            b"poster-bytes",
        ));
        let cache = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;

        let first = cache.get(POSTER).await.expect("first fetch");
        let second = cache.get(POSTER).await.expect("second fetch");

        assert_eq!(first.bytes, second.bytes);
        assert_eq!(first.key, second.key);
        assert_eq!(
            fetcher.call_count(),
            1,
            "the provider must be asked once, not once per request",
        );
    }

    /// The property the whole design exists for: many clients opening the same
    /// grid produce one upstream request per image. The fake is gated so the
    /// followers are provably concurrent with the leader's fetch rather than
    /// merely sequential behind it.
    #[tokio::test]
    async fn concurrent_requests_for_one_image_make_one_upstream_fetch() {
        let root = TempDir::new().unwrap();
        let (fetcher, gate) = InMemoryArtworkFetcher::new()
            .with_image(POSTER, ImageFormat::Png, b"contended")
            .with_gate();
        let fetcher = Arc::new(fetcher);
        let cache = Arc::new(
            cache(
                &root,
                1_000_000,
                fetcher.clone(),
                Arc::new(TestClock::new()),
            )
            .await,
        );

        let requests: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move { cache.get(POSTER).await })
            })
            .collect();

        // One request is inside the provider call; the other seven are queued
        // on its lane rather than opening calls of their own.
        gate.wait_for_fetches(1).await;
        assert_eq!(fetcher.call_count(), 1);
        gate.release();

        for request in requests {
            let served = request.await.expect("task joins").expect("image served");
            assert_eq!(served.bytes.as_ref(), b"contended");
        }
        assert_eq!(fetcher.call_count(), 1, "one fetch for eight requests");
    }

    /// Re-enrichment pointing a title at different artwork is the only
    /// turnover event there is, and it needs no invalidation step: a different
    /// URL is a different key, so it serves fresh bytes under a fresh ETag.
    #[tokio::test]
    async fn artwork_that_moves_upstream_serves_fresh_bytes_under_a_new_key() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(
            InMemoryArtworkFetcher::new()
                .with_image(POSTER, ImageFormat::Jpeg, b"the-old-art")
                .with_image(OTHER, ImageFormat::Jpeg, b"the-new-art"),
        );
        let cache = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;

        let before = cache.get(POSTER).await.expect("old art");
        let after = cache.get(OTHER).await.expect("new art");

        assert_ne!(before.key, after.key, "the ETag must change with the art");
        assert_eq!(after.bytes.as_ref(), b"the-new-art");
        // The stale entry is still cached rather than invalidated; the LRU
        // reclaims it in its own time.
        assert_eq!(
            cache.get(POSTER).await.expect("old art").bytes,
            before.bytes
        );
        assert_eq!(fetcher.call_count(), 2);
    }

    #[tokio::test]
    async fn the_least_recently_used_entry_goes_when_the_cache_is_over_its_ceiling() {
        let root = TempDir::new().unwrap();
        // Each image is 8 bytes and the ceiling holds two of them.
        let fetcher = Arc::new(
            InMemoryArtworkFetcher::new()
                .with_image("https://cdn.test/a.jpg", ImageFormat::Jpeg, b"aaaaaaaa")
                .with_image("https://cdn.test/b.jpg", ImageFormat::Jpeg, b"bbbbbbbb")
                .with_image("https://cdn.test/c.jpg", ImageFormat::Jpeg, b"cccccccc"),
        );
        let cache = cache(&root, 16, fetcher.clone(), Arc::new(TestClock::new())).await;

        cache.get("https://cdn.test/a.jpg").await.expect("a");
        cache.get("https://cdn.test/b.jpg").await.expect("b");
        // Touch `a` so `b` becomes the least recently used one.
        cache.get("https://cdn.test/a.jpg").await.expect("a again");
        cache.get("https://cdn.test/c.jpg").await.expect("c");
        assert_eq!(fetcher.call_count(), 3, "three distinct images so far");

        // `a` survived; `b` was evicted and has to be fetched again.
        cache
            .get("https://cdn.test/a.jpg")
            .await
            .expect("a survives");
        assert_eq!(fetcher.call_count(), 3);

        cache
            .get("https://cdn.test/b.jpg")
            .await
            .expect("b returns");
        assert_eq!(fetcher.call_count(), 4, "the evicted entry was re-fetched");
    }

    /// A restart must not re-fetch a whole library's art, so the index is
    /// rebuilt from the files rather than being lost with the process.
    #[tokio::test]
    async fn a_reopened_cache_serves_what_the_previous_one_wrote() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::WebP,
            b"survives",
        ));

        let first = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;
        let before = first.get(POSTER).await.expect("cached once");
        drop(first);

        let reopened = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;
        let after = reopened.get(POSTER).await.expect("served after restart");

        assert_eq!(after.bytes, before.bytes);
        assert_eq!(
            after.format,
            ImageFormat::WebP,
            "the extension carries the type"
        );
        assert_eq!(fetcher.call_count(), 1, "a restart is not a cache flush");
    }

    /// A directory can hold two formats for one key: an eviction that failed to
    /// unlink a file leaves it behind, and the provider can later answer the
    /// same URL with a different format. Only one of the two is reachable --
    /// the index holds one format per key -- so the other would sit there
    /// unread and unevictable, keeping the cache above its ceiling for good.
    #[tokio::test]
    async fn a_key_left_with_two_formats_keeps_only_the_one_it_can_serve() {
        let root = TempDir::new().unwrap();
        let key = CacheKey::for_url(POSTER);
        let shard = root.path().join("artwork").join(&key.as_str()[..2]);
        tokio::fs::create_dir_all(&shard).await.unwrap();
        for (extension, bytes) in [("jpg", b"as-jpeg"), ("webp", b"as-webp")] {
            tokio::fs::write(shard.join(format!("{key}.{extension}")), bytes)
                .await
                .unwrap();
        }

        // An empty provider, so anything served has to have come off disk.
        let fetcher = Arc::new(InMemoryArtworkFetcher::new());
        let cache = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;

        let mut left = Vec::new();
        let mut files = tokio::fs::read_dir(&shard).await.unwrap();
        while let Some(file) = files.next_entry().await.unwrap() {
            left.push(file.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(
            left.len(),
            1,
            "the unreachable duplicate must not survive the scan, found {left:?}",
        );

        let served = cache.get(POSTER).await.expect("the survivor is served");
        assert_eq!(
            fetcher.call_count(),
            0,
            "it came off disk, not the provider"
        );
        // Derived from what was served rather than named outright: the point is
        // that the file left on disk is the one the index points at, whichever
        // of the two that turned out to be.
        assert_eq!(
            left[0],
            format!("{key}.{}", served.format.extension()),
            "the survivor must be the format the cache serves",
        );
    }

    #[tokio::test]
    async fn a_failing_provider_is_asked_once_per_ttl_not_once_per_request() {
        let root = TempDir::new().unwrap();
        let fetcher =
            Arc::new(InMemoryArtworkFetcher::new().with_error(POSTER, ArtworkFetchError::NotFound));
        let clock = Arc::new(TestClock::new());
        let cache = cache(&root, 1_000_000, fetcher.clone(), clock.clone()).await;

        assert_eq!(
            cache.get(POSTER).await.unwrap_err(),
            ArtworkFetchError::NotFound
        );
        assert_eq!(
            cache.get(POSTER).await.unwrap_err(),
            ArtworkFetchError::NotFound
        );
        assert_eq!(fetcher.call_count(), 1, "the failure is remembered");

        clock.advance(TTL);
        assert_eq!(
            cache.get(POSTER).await.unwrap_err(),
            ArtworkFetchError::NotFound
        );
        assert_eq!(fetcher.call_count(), 2, "and forgotten once it expires");
    }

    /// Something outside Beam clearing the cache directory must degrade into a
    /// re-fetch, not into an entry that is indexed forever and never readable.
    #[tokio::test]
    async fn an_indexed_file_that_vanished_is_re_fetched() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            b"poster",
        ));
        let cache = cache(
            &root,
            1_000_000,
            fetcher.clone(),
            Arc::new(TestClock::new()),
        )
        .await;

        cache.get(POSTER).await.expect("cached");
        let key = CacheKey::for_url(POSTER);
        let shard: String = key.as_str().chars().take(2).collect();
        let path = root
            .path()
            .join("artwork")
            .join(shard)
            .join(format!("{}.jpg", key.as_str()));
        std::fs::remove_file(&path).expect("file was where the cache put it");

        let served = cache.get(POSTER).await.expect("re-fetched");
        assert_eq!(served.bytes.as_ref(), b"poster");
        assert_eq!(fetcher.call_count(), 2);
    }

    fn width(pixels: u32) -> NonZeroU32 {
        NonZeroU32::new(pixels).expect("a positive width")
    }

    /// A real JPEG of `width` by `height`, so a resize has pixels to work on.
    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let pixels = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&pixels)
            .expect("the fixture encodes");
        encoded
    }

    /// A real PNG of `width` by `height` that is half transparent.
    fn translucent_png(width: u32, height: u32) -> Vec<u8> {
        let pixels = image::RgbaImage::from_fn(width, height, |x, _| {
            image::Rgba([255, 0, 0, if x % 2 == 0 { 0 } else { 255 }])
        });
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .expect("the fixture encodes");
        encoded
    }

    fn decoded(bytes: &[u8]) -> image::DynamicImage {
        image::load_from_memory(bytes).expect("a decodable image")
    }

    async fn sized_cache(
        root: &TempDir,
        max_bytes: u64,
        fetcher: &Arc<InMemoryArtworkFetcher>,
    ) -> ArtworkCache {
        cache(
            root,
            max_bytes,
            Arc::clone(fetcher),
            Arc::new(TestClock::new()),
        )
        .await
    }

    #[tokio::test]
    async fn a_size_is_resized_once_and_cached_under_its_own_key() {
        let root = TempDir::new().unwrap();
        let original = jpeg(1000, 1500);
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            &original,
        ));
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        let sized = cache
            .get_sized(POSTER, width(342))
            .await
            .expect("a rendition");
        let again = cache
            .get_sized(POSTER, width(342))
            .await
            .expect("the same rendition");

        let rendition = decoded(&sized.bytes);
        assert_eq!((rendition.width(), rendition.height()), (342, 513));
        assert_eq!(sized.format, ImageFormat::Jpeg);
        assert!(sized.bytes.len() < original.len());
        assert_eq!(sized.key, CacheKey::for_sized(POSTER, width(342)));
        assert_ne!(sized.key, CacheKey::for_url(POSTER));
        assert_ne!(
            CacheKey::for_sized(POSTER, width(185)),
            sized.key,
            "each size is its own entry",
        );
        assert_eq!(again.bytes, sized.bytes);
        assert_eq!(fetcher.call_count(), 1);

        let file = root
            .path()
            .join("artwork")
            .join(&sized.key.as_str()[..2])
            .join(format!("{}.jpg", sized.key));
        assert_eq!(
            std::fs::read(&file).expect("the rendition is on disk"),
            sized.bytes.as_ref(),
        );
    }

    #[tokio::test]
    async fn a_reopened_cache_serves_a_size_without_the_provider_or_the_original() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            &jpeg(800, 1200),
        ));
        let before = sized_cache(&root, 10_000_000, &fetcher)
            .await
            .get_sized(POSTER, width(185))
            .await
            .expect("a rendition");

        // Nothing to fetch from: the reopened cache must find it on disk.
        let offline = Arc::new(InMemoryArtworkFetcher::new());
        let after = sized_cache(&root, 10_000_000, &offline)
            .await
            .get_sized(POSTER, width(185))
            .await
            .expect("served after restart");

        assert_eq!(after.bytes, before.bytes);
        assert_eq!(after.key, before.key);
        assert_eq!(offline.call_count(), 0);
    }

    #[tokio::test]
    async fn an_original_no_wider_than_the_size_is_served_as_it_is() {
        let root = TempDir::new().unwrap();
        let original = jpeg(185, 278);
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            &original,
        ));
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        let sized = cache
            .get_sized(POSTER, width(342))
            .await
            .expect("the original");
        let exact = cache
            .get_sized(POSTER, width(185))
            .await
            .expect("the original");

        for served in [&sized, &exact] {
            assert_eq!(served.bytes.as_ref(), original.as_slice(), "never upscaled");
            assert_eq!(
                served.key,
                CacheKey::for_url(POSTER),
                "the original's bytes keep the original's validator",
            );
        }
    }

    #[tokio::test]
    async fn an_image_beam_cannot_resize_is_served_at_its_original_size() {
        const ANIMATED: &str = "https://image.tmdb.org/t/p/w500/animated.gif";
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(
            InMemoryArtworkFetcher::new()
                .with_image(POSTER, ImageFormat::Jpeg, b"\xff\xd8\xff-truncated")
                .with_image(ANIMATED, ImageFormat::Gif, b"GIF89a-not-decoded"),
        );
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        for (case, url, bytes) in [
            (
                "bytes that do not decode",
                POSTER,
                &b"\xff\xd8\xff-truncated"[..],
            ),
            (
                "a format Beam does not resize",
                ANIMATED,
                b"GIF89a-not-decoded",
            ),
        ] {
            for _ in 0..2 {
                let served = cache
                    .get_sized(url, width(185))
                    .await
                    .unwrap_or_else(|err| panic!("{case}: {err}"));
                assert_eq!(served.bytes.as_ref(), bytes, "{case}");
                assert_eq!(served.key, CacheKey::for_url(url), "{case}");
            }
        }
        assert_eq!(fetcher.call_count(), 2, "each original is fetched once");
    }

    /// A few hundred bytes can declare dimensions whose pixels would not fit
    /// in memory. Past the decode ceiling Beam does not decode at all: the
    /// original is served, never a rendition.
    #[tokio::test]
    async fn an_image_past_the_decode_ceiling_is_not_decoded() {
        let root = TempDir::new().unwrap();
        let oversized = jpeg(12_001, 1);
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            &oversized,
        ));
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        let served = cache
            .get_sized(POSTER, width(185))
            .await
            .expect("the original");

        assert_eq!(served.bytes.as_ref(), oversized.as_slice());
        assert_eq!(served.key, CacheKey::for_url(POSTER));
    }

    #[tokio::test]
    async fn transparency_survives_a_resize() {
        let root = TempDir::new().unwrap();
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Png,
            &translucent_png(600, 300),
        ));
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        let sized = cache
            .get_sized(POSTER, width(185))
            .await
            .expect("a rendition");

        assert_eq!(sized.format, ImageFormat::Png);
        let rendition = decoded(&sized.bytes);
        assert_eq!((rendition.width(), rendition.height()), (185, 93));
        assert!(rendition.color().has_alpha());
    }

    #[tokio::test]
    async fn an_original_the_provider_does_not_have_fails_every_size() {
        let root = TempDir::new().unwrap();
        let fetcher =
            Arc::new(InMemoryArtworkFetcher::new().with_error(POSTER, ArtworkFetchError::NotFound));
        let cache = sized_cache(&root, 10_000_000, &fetcher).await;

        let err = cache
            .get_sized(POSTER, width(185))
            .await
            .expect_err("no original, no rendition");

        assert_eq!(err, ArtworkFetchError::NotFound);
    }

    /// Sizes share the one ceiling: a rendition is an LRU entry like any
    /// other, and storing it can evict the original it was made from.
    #[tokio::test]
    async fn a_size_counts_against_the_cache_ceiling() {
        let root = TempDir::new().unwrap();
        let original = jpeg(1000, 1500);
        let fetcher = Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER,
            ImageFormat::Jpeg,
            &original,
        ));
        let cache = sized_cache(&root, original.len() as u64, &fetcher).await;

        cache
            .get_sized(POSTER, width(185))
            .await
            .expect("a rendition");

        let original_file = root
            .path()
            .join("artwork")
            .join(&CacheKey::for_url(POSTER).as_str()[..2])
            .join(format!("{}.jpg", CacheKey::for_url(POSTER)));
        assert!(
            !original_file.exists(),
            "the original was least recently used, so it went",
        );
        cache.get(POSTER).await.expect("the original, re-fetched");
        assert_eq!(fetcher.call_count(), 2);
    }
}
