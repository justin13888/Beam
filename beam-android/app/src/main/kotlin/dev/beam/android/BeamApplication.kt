package dev.beam.android

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import dagger.hilt.android.HiltAndroidApp
import dev.beam.android.core.media.http.BeamImageLoader
import dev.beam.android.core.media.http.ServerCallFactory
import dev.beam.android.sync.ProgressFlusher
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.launch
import javax.inject.Inject

/** The application. */
@HiltAndroidApp
public class BeamApplication :
    Application(),
    SingletonImageLoader.Factory {
    /**
     * Fetches as the signed-in user from the active server.
     *
     * Load-bearing rather than tidy: posters and backdrops are served from the
     * same authenticated origin as everything else, so an image loader without
     * the session cookie would get a 401 for every poster. It also applies the
     * trust decision, so artwork does not fail on exactly the self-signed
     * servers the trust prompt exists for.
     */
    @Inject
    internal lateinit var serverCalls: ServerCallFactory

    /** Sends positions recorded while offline, for as long as the process lives. */
    @Inject
    internal lateinit var progressFlusher: ProgressFlusher

    override fun onCreate() {
        super.onCreate()
        // Process-wide rather than tied to a screen: a reconnect while the app
        // sits in the background is as good a moment to drain as any.
        MainScope().launch { progressFlusher.run() }
    }

    override fun newImageLoader(context: PlatformContext): ImageLoader =
        BeamImageLoader.build(
            context = context,
            calls = serverCalls,
            diskCacheDirectory = cacheDir.resolve("artwork"),
        )
}
