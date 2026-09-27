package dev.beam.android

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import dagger.hilt.android.HiltAndroidApp
import dev.beam.android.core.media.http.BeamImageLoader
import dev.beam.android.core.media.http.ServerCallFactory
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

    override fun newImageLoader(context: PlatformContext): ImageLoader =
        BeamImageLoader.build(
            context = context,
            calls = serverCalls,
            diskCacheDirectory = cacheDir.resolve("artwork"),
        )
}
