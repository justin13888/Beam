// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.session

import android.app.PendingIntent
import android.content.Intent
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import dagger.hilt.android.AndroidEntryPoint
import javax.inject.Inject

/**
 * Keeps playback alive when the app is not in front.
 *
 * A [MediaSessionService] is what makes the lock-screen and notification
 * controls, Bluetooth and headset buttons, and Android Auto work -- none of
 * which a bare [ExoPlayer] provides. It also owns the foreground notification,
 * which is what stops the system killing playback the moment the activity goes
 * away.
 */
@AndroidEntryPoint
public class PlaybackService : MediaSessionService() {
    /**
     * Injected rather than constructed here so the player the service publishes
     * and the player the UI drives are the same instance. Two players would
     * mean the notification and the screen disagreeing about what is playing.
     */
    @Inject
    internal lateinit var playerProvider: PlayerProvider

    private var mediaSession: MediaSession? = null

    override fun onCreate() {
        super.onCreate()
        val player = playerProvider.exoPlayer()
        player.setAudioAttributes(
            AudioAttributes
                .Builder()
                .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                .setUsage(C.USAGE_MEDIA)
                .build(),
            // Handle audio focus: pause for a phone call rather than talking
            // over it, and duck for a navigation prompt.
            // handleAudioFocus =
            true,
        )
        mediaSession =
            MediaSession
                .Builder(this, player)
                .setSessionActivity(openAppIntent())
                .build()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = mediaSession

    override fun onTaskRemoved(rootIntent: Intent?) {
        // Swiping the app away stops playback only when it is already paused.
        // Killing active playback on a swipe is the behaviour every media app
        // is asked to stop doing: the viewer dismissed the task, not the audio.
        val player = mediaSession?.player
        if (player == null || !player.playWhenReady || player.mediaItemCount == 0) {
            stopSelf()
        }
    }

    override fun onDestroy() {
        mediaSession?.run {
            player.release()
            release()
        }
        mediaSession = null
        super.onDestroy()
    }

    private fun openAppIntent(): PendingIntent {
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        return PendingIntent.getActivity(
            this,
            0,
            launch,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }
}

/** Hands out the one [ExoPlayer] the app owns. */
public interface PlayerProvider {
    /** The shared player instance. */
    public fun exoPlayer(): ExoPlayer
}
