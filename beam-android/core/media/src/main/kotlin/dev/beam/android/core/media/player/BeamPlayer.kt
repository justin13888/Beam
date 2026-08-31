package dev.beam.android.core.media.player

import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.flow.StateFlow

/**
 * The player, as every screen above it sees one.
 *
 * An interface rather than ExoPlayer directly, for the reason the whole module
 * graph is shaped this way: the player screen's tests then run on the JVM
 * against a fake, with no `MediaCodec`, no `Looper`, and no native library.
 * Robolectric can host ExoPlayer, but not decode, so a fake is the only way to
 * assert on what happens *after* playback ends -- which is where the
 * interesting behaviour lives.
 */
public interface BeamPlayer {
    /** What the player is doing now. */
    public val state: StateFlow<PlaybackUiState>

    /** Load a title and begin, resuming from [PlaybackRequest.startPositionSecs]. */
    public suspend fun play(request: PlaybackRequest)

    /** Resume after a pause. */
    public fun resume()

    /** Pause, and report the position immediately. */
    public fun pause()

    /** Seek to an absolute position, and report it immediately. */
    public fun seekTo(positionMs: Long)

    /** Move by a signed delta, clamped to the item. */
    public fun seekBy(deltaMs: Long)

    /** Set playback speed, where 1.0 is normal. */
    public fun setSpeed(speed: Float)

    /** Choose an audio track by [TrackOption.id]. */
    public fun selectAudioTrack(id: String)

    /** Choose a subtitle track, or null to turn subtitles off. */
    public fun selectSubtitleTrack(id: String?)

    /**
     * Switch to a different file of the same title, holding position.
     *
     * A discrete action, never automatic: Beam does not transcode and does not
     * do adaptive bitrate ([ADR-0004]), so changing quality means changing
     * which file is being read. Position is preserved because the alternative
     * -- restarting the title because the viewer wanted a smaller file -- is
     * indefensible.
     */
    public suspend fun switchSource(fileId: String)

    /** Stop, report the final position, and release the decoder. */
    public fun stop()

    /** Release everything. The instance is unusable afterwards. */
    public fun release()
}
