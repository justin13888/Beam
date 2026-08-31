package dev.beam.android.feature.player

import android.app.Activity
import android.view.WindowManager
import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.delay

/** The player, wired to its view model. */
@Composable
public fun PlayerRoute(
    request: PlaybackRequest,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: PlayerViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val activity = LocalActivity.current

    LaunchedEffect(request.fileId) {
        viewModel.start(request)
    }

    // The screen must not dim during a film. Held only while this screen is
    // composed and while something is actually playing, so a paused player
    // does not keep the display awake indefinitely.
    DisposableEffect(activity, state.playback.isPlaying) {
        val window = (activity as? Activity)?.window
        if (state.playback.isPlaying) {
            window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        onDispose {
            window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    // Controls fade out on their own while playing, and stay put while paused
    // -- a viewer who paused is usually looking at the frame, not waiting for
    // the chrome to disappear.
    LaunchedEffect(state.areControlsVisible, state.playback.isPlaying) {
        if (state.areControlsVisible && state.playback.isPlaying) {
            delay(CONTROLS_TIMEOUT_MS)
            viewModel.setControlsVisible(false)
        }
    }

    Box(
        modifier =
            modifier
                .fillMaxSize()
                .background(Color.Black)
                .pointerInput(Unit) {
                    detectTapGestures(
                        onTap = { viewModel.setControlsVisible(!state.areControlsVisible) },
                    )
                },
    ) {
        VideoSurface(player = viewModel.exoPlayer, modifier = Modifier.fillMaxSize())

        PlayerControls(
            state = state,
            onTogglePlayPause = viewModel::togglePlayPause,
            onSeek = viewModel::seekTo,
            onRewind = viewModel::rewind,
            onFastForward = viewModel::fastForward,
            onOpenSettings = { viewModel.setControlsVisible(true) },
            onClose = {
                viewModel.stop()
                onClose()
            },
            onPlayNext = viewModel::playNext,
            onDismissNext = viewModel::dismissUpNext,
        )
    }
}

private const val CONTROLS_TIMEOUT_MS = 3_500L
