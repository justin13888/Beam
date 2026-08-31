package dev.beam.android

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.disk.DiskCache
import coil3.memory.MemoryCache
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import coil3.request.crossfade
import dagger.hilt.android.HiltAndroidApp
import okhttp3.OkHttpClient
import okio.Path.Companion.toOkioPath
import javax.inject.Inject

/** The application. */
@HiltAndroidApp
public class BeamApplication :
    Application(),
    SingletonImageLoader.Factory {
    /**
     * The same client the API and playback use.
     *
     * Load-bearing rather than tidy: posters and backdrops are served from the
     * same authenticated origin as everything else, so an image loader with
     * its own client would send no session cookie and every poster would be a
     * 401. It also inherits the trust decision, so artwork does not fail on
     * exactly the self-signed servers the trust prompt exists for.
     */
    @Inject
    internal lateinit var okHttpClient: OkHttpClient

    override fun newImageLoader(context: PlatformContext): ImageLoader =
        ImageLoader
            .Builder(context)
            .components {
                add(OkHttpNetworkFetcherFactory(callFactory = { okHttpClient }))
            }.memoryCache {
                MemoryCache
                    .Builder()
                    .maxSizePercent(context, MEMORY_CACHE_FRACTION)
                    .build()
            }.diskCache {
                DiskCache
                    .Builder()
                    .directory(cacheDir.resolve("artwork").toOkioPath())
                    .maxSizeBytes(DISK_CACHE_BYTES)
                    .build()
            }.crossfade(true)
            .build()

    private companion object {
        /**
         * A grid of posters is the memory-hungriest thing the app renders, and
         * artwork that has to be re-decoded on every scroll is what makes a
         * list feel cheap.
         */
        const val MEMORY_CACHE_FRACTION = 0.25

        const val DISK_CACHE_BYTES = 256L * 1024 * 1024
    }
}
