// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.feature.player

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi

/**
 * The surface the decoder renders into.
 *
 * A `PlayerView` with its own controls switched off, rather than Compose's
 * `PlayerSurface`: `PlayerView` handles the surface lifecycle, the aspect
 * ratio, and subtitle rendering, and getting any of those wrong shows up as a
 * black frame or stretched video rather than as an error. The chrome is
 * Compose's, so nothing of the platform look leaks through.
 */
@Composable
internal fun VideoSurface(
    player: Player?,
    modifier: Modifier = Modifier,
) {
    AndroidView(
        modifier = modifier,
        factory = { context ->
            androidx.media3.ui.PlayerView(context).apply {
                useController = false
                setShutterBackgroundColor(android.graphics.Color.BLACK)
            }
        },
        update = { view -> view.player = player },
        onRelease = { view -> view.player = null },
    )
}
