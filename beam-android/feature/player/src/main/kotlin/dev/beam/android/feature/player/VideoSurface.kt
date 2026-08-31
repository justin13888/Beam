package dev.beam.android.feature.player

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.Player

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
