package dev.beam.android.core.media.download

import android.content.Context
import coil3.ImageLoader
import coil3.request.CachePolicy
import coil3.request.ImageRequest

/**
 * Keeps a download's poster on disk for roughly as long as the download exists.
 *
 * The downloads screen is the one screen guaranteed to be used offline, and a
 * poster URL written down at enqueue time is useless there unless its bytes
 * were fetched while the network was still up. So enqueueing fetches them into
 * the image loader's disk cache, and removing the download evicts them.
 *
 * It goes through the app's own [ImageLoader] rather than a second fetcher for
 * two reasons. That loader fetches through
 * [dev.beam.android.core.media.http.ServerCallFactory], which attaches the
 * active server's session cookie and trust decision, so the poster is fetched
 * under the same credential as the API -- `/v1/artwork` answers 401 without
 * it. And an entry written to its disk cache is exactly the entry `Artwork`
 * reads on render: the disk cache key is the URL, which is also what the
 * screen passes as its model, so nothing has to keep two copies or two key
 * schemes in step.
 *
 * The cost of sharing that cache is that the poster's lifetime approximates
 * the download's rather than matching it: the cache is an LRU with a size
 * cap, so enough browsing can still push a pinned poster out. Every render of
 * the downloads screen reads the entry and so refreshes it, and a poster that
 * is pushed out anyway falls back to the placeholder offline and is fetched
 * again on the next online render -- never a failed download. A poster that
 * must survive regardless would need a store of its own outside the cache.
 *
 * @param loader resolved on first use rather than at construction, because the
 *   app's singleton loader is built from a client that is injected into the
 *   application after the graph that builds this class.
 */
public class DownloadArtwork(
    private val context: Context,
    private val loader: () -> ImageLoader,
) {
    /**
     * Fetch [url] into the disk cache.
     *
     * Never throws for a network failure: a poster is decoration on a download
     * the viewer asked for, and failing the download because the poster would
     * not load would be the wrong way round.
     */
    public suspend fun pin(url: String) {
        val request =
            ImageRequest
                .Builder(context)
                .data(url)
                .diskCacheKey(url)
                // The point is the bytes on disk. Filling the memory cache
                // would evict artwork the viewer is actually looking at in
                // exchange for a poster nobody is looking at yet.
                .memoryCachePolicy(CachePolicy.DISABLED)
                .diskCachePolicy(CachePolicy.ENABLED)
                // Coil caches the undecoded response, so the decode that
                // follows the fetch is pure cost here. A tiny target makes it
                // a subsampled decode of a few pixels instead of a full poster.
                .size(PIN_DECODE_SIZE_PX, PIN_DECODE_SIZE_PX)
                .build()
        // The result is deliberately not inspected: a failure leaves nothing
        // on disk, and the next enqueue of the same file, or the downloads
        // screen rendering while online, fetches it again.
        loader().execute(request)
    }

    /** Stop keeping [url]; it is fetched again on the next online render. */
    public fun unpin(url: String) {
        loader().diskCache?.remove(url)
    }

    private companion object {
        const val PIN_DECODE_SIZE_PX = 1
    }
}
