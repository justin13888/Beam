package dev.beam.android.core.media.http

import coil3.ImageLoader
import coil3.PlatformContext
import coil3.disk.DiskCache
import coil3.memory.MemoryCache
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import coil3.request.crossfade
import okio.Path.Companion.toOkioPath
import java.io.File

/**
 * The app's one image loader, as the application hands it to Coil.
 *
 * Built here rather than in the application so the wiring that makes artwork
 * work -- every fetch going through [ServerCallFactory] -- is the same object
 * the tests exercise, not a copy of it.
 */
public object BeamImageLoader {
    /**
     * @param calls the only network path. Artwork is served from the same
     *   authenticated origin as the API, so a loader on any other client sends
     *   no session and every poster is a 401 -- and fails on exactly the
     *   self-signed servers the trust prompt exists for.
     * @param diskCacheDirectory where fetched artwork is kept. Also where a
     *   download's poster is pinned (see `DownloadArtwork`), so it must be the
     *   same directory across launches.
     */
    public fun build(
        context: PlatformContext,
        calls: ServerCallFactory,
        diskCacheDirectory: File,
    ): ImageLoader =
        ImageLoader
            .Builder(context)
            .components {
                add(OkHttpNetworkFetcherFactory(callFactory = { calls }))
            }.memoryCache {
                MemoryCache
                    .Builder()
                    .maxSizePercent(context, MEMORY_CACHE_FRACTION)
                    .build()
            }.diskCache {
                DiskCache
                    .Builder()
                    .directory(diskCacheDirectory.toOkioPath())
                    .maxSizeBytes(DISK_CACHE_BYTES)
                    .build()
            }.crossfade(true)
            .build()

    /**
     * A grid of posters is the memory-hungriest thing the app renders, and
     * artwork that has to be re-decoded on every scroll is what makes a list
     * feel cheap.
     */
    private const val MEMORY_CACHE_FRACTION = 0.25

    private const val DISK_CACHE_BYTES = 256L * 1024 * 1024
}
