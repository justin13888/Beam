package dev.beam.android.feature.player

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Forward30
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Replay10
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.media.player.PlaybackUiState
import dev.beam.android.core.ui.Format

/**
 * The transport controls, over whatever surface is rendering the video.
 *
 * The surface itself is supplied by the caller, because it is a platform view
 * whose lifetime belongs to the activity -- and because that keeps this
 * composable renderable in a screenshot test, which a `SurfaceView` is not.
 */
@Composable
internal fun PlayerControls(
    state: PlayerScreenState,
    onTogglePlayPause: () -> Unit,
    onSeek: (Long) -> Unit,
    onRewind: () -> Unit,
    onFastForward: () -> Unit,
    onOpenSettings: () -> Unit,
    onClose: () -> Unit,
    onPlayNext: () -> Unit,
    onDismissNext: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val playback = state.playback

    Box(modifier = modifier.fillMaxSize()) {
        if (playback.isBuffering) {
            CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
        }

        playback.error?.let { failure ->
            Column(
                modifier =
                    Modifier
                        .align(Alignment.Center)
                        .padding(BeamSpacing.Large),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(BeamSpacing.Medium),
            ) {
                Text(text = failure.message, style = MaterialTheme.typography.bodyLarge)
                if (failure.suggestsAnotherSource) {
                    // The only useful action for a decoder failure: Beam never
                    // transcodes, so retrying the same file fails identically.
                    Button(onClick = onOpenSettings) { Text("Choose another source") }
                }
            }
        }

        if (state.areControlsVisible) {
            IconButton(
                onClick = onClose,
                modifier =
                    Modifier
                        .align(Alignment.TopStart)
                        .safeDrawingPadding(),
            ) {
                Icon(Icons.Rounded.Close, contentDescription = "Close the player")
            }

            IconButton(
                onClick = onOpenSettings,
                modifier =
                    Modifier
                        .align(Alignment.TopEnd)
                        .safeDrawingPadding(),
            ) {
                Icon(Icons.Rounded.Tune, contentDescription = "Playback settings")
            }

            Row(
                modifier = Modifier.align(Alignment.Center),
                horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Large),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                IconButton(onClick = onRewind) {
                    Icon(Icons.Rounded.Replay10, contentDescription = "Back 10 seconds")
                }
                IconButton(onClick = onTogglePlayPause) {
                    Icon(
                        imageVector =
                            if (playback.isPlaying) {
                                Icons.Rounded.Pause
                            } else {
                                Icons.Rounded.PlayArrow
                            },
                        contentDescription = if (playback.isPlaying) "Pause" else "Play",
                    )
                }
                IconButton(onClick = onFastForward) {
                    Icon(Icons.Rounded.Forward30, contentDescription = "Forward 30 seconds")
                }
            }

            Scrubber(
                playback = playback,
                onSeek = onSeek,
                modifier =
                    Modifier
                        .align(Alignment.BottomCenter)
                        .safeDrawingPadding()
                        .padding(horizontal = BeamSpacing.Medium),
            )
        }

        if (state.isOfferingNext) {
            state.upNext?.let { next ->
                Column(
                    modifier =
                        Modifier
                            .align(Alignment.BottomEnd)
                            .safeDrawingPadding()
                            .padding(BeamSpacing.Large)
                            .background(
                                MaterialTheme.colorScheme.surface.copy(alpha = 0.92f),
                                MaterialTheme.shapes.large,
                            ).padding(BeamSpacing.Medium),
                    verticalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
                ) {
                    Text("Up next", style = MaterialTheme.typography.labelMedium)
                    Text(next.title, style = MaterialTheme.typography.titleMedium)
                    Row(horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small)) {
                        TextButton(onClick = onDismissNext) { Text("Not now") }
                        Button(onClick = onPlayNext) { Text("Play") }
                    }
                }
            }
        }
    }
}

@Composable
private fun Scrubber(
    playback: PlaybackUiState,
    onSeek: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    // While the viewer is dragging, the slider follows the finger rather than
    // the player: reading the position back mid-drag makes the thumb fight the
    // gesture and snap backwards on every frame.
    var scrubbing by remember { mutableStateOf<Float?>(null) }
    val duration = playback.durationMs.takeIf { it > 0L }
    val fraction = scrubbing ?: playback.progress ?: 0f

    Column(modifier = modifier.fillMaxWidth()) {
        Slider(
            value = fraction,
            onValueChange = { scrubbing = it },
            onValueChangeFinished = {
                val target = scrubbing
                scrubbing = null
                if (duration != null && target != null) {
                    onSeek((duration * target).toLong())
                }
            },
            enabled = duration != null,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                text =
                    Format.timecode(
                        ((duration?.times(fraction))?.toLong() ?: playback.positionMs) / 1_000.0,
                    ),
                style = MaterialTheme.typography.labelSmall,
            )
            Text(
                text = duration?.let { Format.timecode(it / 1_000.0) } ?: "",
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
}
