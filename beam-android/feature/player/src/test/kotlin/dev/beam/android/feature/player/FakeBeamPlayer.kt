package dev.beam.android.feature.player

import dev.beam.android.core.media.player.BeamPlayer
import dev.beam.android.core.media.player.PlaybackUiState
import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * A player that records what it was told to do.
 *
 * The reason the [BeamPlayer] interface exists: Robolectric can host an
 * ExoPlayer but cannot decode, so the behaviour worth asserting here -- what
 * happens when a title ends -- is only reachable through a fake that can be
 * driven straight to that state.
 */
internal class FakeBeamPlayer : BeamPlayer {
    private val mutableState = MutableStateFlow(PlaybackUiState())
    override val state: StateFlow<PlaybackUiState> = mutableState

    /** Every request passed to [play], in order. */
    val played: MutableList<PlaybackRequest> = mutableListOf()

    /** Every file id passed to [switchSource]. */
    val switched: MutableList<String> = mutableListOf()

    /** Every seek delta, in milliseconds. */
    val seeks: MutableList<Long> = mutableListOf()

    /** Whether [stop] was called. */
    var stopped: Boolean = false
        private set

    /** Whether [release] was called. */
    var released: Boolean = false
        private set

    override suspend fun play(request: PlaybackRequest) {
        played += request
        mutableState.value = PlaybackUiState(request = request, isPlaying = true)
    }

    override fun resume() {
        mutableState.value = mutableState.value.copy(isPlaying = true)
    }

    override fun pause() {
        mutableState.value = mutableState.value.copy(isPlaying = false)
    }

    override fun seekTo(positionMs: Long) {
        seeks += positionMs
        mutableState.value = mutableState.value.copy(positionMs = positionMs)
    }

    override fun seekBy(deltaMs: Long) {
        seeks += deltaMs
    }

    override fun setSpeed(speed: Float) {
        mutableState.value = mutableState.value.copy(speed = speed)
    }

    override fun selectAudioTrack(id: String) = Unit

    override fun selectSubtitleTrack(id: String?) = Unit

    override suspend fun switchSource(fileId: String) {
        switched += fileId
    }

    override fun stop() {
        stopped = true
    }

    override fun release() {
        released = true
    }

    /** Drive the player to the end of the current item. */
    fun finish() {
        mutableState.value = mutableState.value.copy(hasEnded = true, isPlaying = false)
    }
}
