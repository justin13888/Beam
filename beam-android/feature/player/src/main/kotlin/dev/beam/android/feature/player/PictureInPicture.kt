package dev.beam.android.feature.player

import android.app.PictureInPictureParams
import android.content.pm.PackageManager
import android.graphics.Rect
import android.os.Build
import android.util.Rational
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.core.util.Consumer
import androidx.lifecycle.Lifecycle

/**
 * Picture-in-picture for the player screen.
 *
 * While something plays, leaving the app -- the home gesture, another app --
 * shrinks the film into a window instead of stopping it: on Android 12 and
 * later by the system's own auto-enter, earlier by entering on the
 * user-leave hint. Paused, leaving is leaving. The rule is only ever in force
 * while this screen is composed, so the rest of the app never turns into a
 * window.
 *
 * [onModeChanged] hears every change, so the screen can draw the picture
 * alone; [onDismissed] hears the viewer closing the window, which is stopping.
 */
@Composable
internal fun PictureInPicture(
    isPlaying: Boolean,
    sourceRectHint: Rect?,
    onModeChanged: (Boolean) -> Unit,
    onDismissed: () -> Unit,
) {
    val activity = LocalActivity.current as? ComponentActivity ?: return
    val supported =
        remember(activity) {
            activity.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)
        }
    if (!supported) return

    val playing by rememberUpdatedState(isPlaying)
    val modeChanged by rememberUpdatedState(onModeChanged)
    val dismissed by rememberUpdatedState(onDismissed)

    val hint by rememberUpdatedState(sourceRectHint)

    DisposableEffect(activity, isPlaying, sourceRectHint) {
        activity.setPictureInPictureParams(pictureInPictureParams(autoEnter = isPlaying, sourceRectHint))
        onDispose { }
    }

    DisposableEffect(activity) {
        // Before Android 12 there is no auto-enter: the hint that the viewer
        // is leaving is the moment to shrink.
        val leaving =
            Runnable {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S && playing) {
                    runCatching { activity.enterPictureInPictureMode(pictureInPictureParams(autoEnter = true, hint)) }
                }
            }
        val changes =
            Consumer<PictureInPictureModeChangedInfo> { info ->
                modeChanged(info.isInPictureInPictureMode)
                // Leaving the window with the activity stopped is the window
                // being closed, not expanded back to full screen.
                if (!info.isInPictureInPictureMode &&
                    !activity.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
                ) {
                    dismissed()
                }
            }
        activity.addOnUserLeaveHintListener(leaving)
        activity.addOnPictureInPictureModeChangedListener(changes)
        modeChanged(activity.isInPictureInPictureMode)
        onDispose {
            activity.removeOnUserLeaveHintListener(leaving)
            activity.removeOnPictureInPictureModeChangedListener(changes)
            // Off the player screen nothing should shrink into a window.
            activity.setPictureInPictureParams(pictureInPictureParams(autoEnter = false, sourceRectHint = null))
        }
    }
}

/**
 * The window's shape and how it is entered. [sourceRectHint] is where the
 * picture is on screen, which the system animates the window out of; without
 * it the shrink starts from the whole screen.
 */
private fun pictureInPictureParams(
    autoEnter: Boolean,
    sourceRectHint: Rect?,
): PictureInPictureParams =
    PictureInPictureParams
        .Builder()
        .setAspectRatio(Rational(16, 9))
        .setSourceRectHint(sourceRectHint)
        .apply {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                setAutoEnterEnabled(autoEnter)
                setSeamlessResizeEnabled(true)
            }
        }.build()
