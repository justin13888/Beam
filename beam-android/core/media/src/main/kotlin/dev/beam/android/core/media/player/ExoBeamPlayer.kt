// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.player

import android.content.Context
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import dev.beam.android.core.ffi.BeamFailure
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.beamDataSourceFactory
import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException

/**
 * [BeamPlayer] over Media3's [ExoPlayer].
 *
 * Everything here runs on the main thread, because that is where the player's
 * `Looper` is; the suspending members only leave it to ask the core for a
 * playback configuration.
 */
internal class ExoBeamPlayer(
    private val context: Context,
    private val exoPlayer: ExoPlayer,
    private val repository: PlaybackRepository,
    private val clients: BeamHttpClientFactory,
    private val scope: CoroutineScope,
) : BeamPlayer,
    Player.Listener {
    private val mutableState = MutableStateFlow(PlaybackUiState())
    override val state: StateFlow<PlaybackUiState> = mutableState.asStateFlow()

    private val reporter = ProgressReporter(repository, scope)
    private var positionTicker: Job? = null
    private var currentFileId: String? = null

    init {
        exoPlayer.addListener(this)
    }

    override suspend fun play(request: PlaybackRequest) {
        val started = prepare(request, request.startPositionSecs)
        if (started) {
            exoPlayer.playWhenReady = true
        }
    }

    override fun resume() {
        exoPlayer.playWhenReady = true
        currentFileId?.let { reporter.start(it, ::sample) }
    }

    override fun pause() {
        exoPlayer.playWhenReady = false
        reporter.stop()
        forceReport()
    }

    override fun seekTo(positionMs: Long) {
        exoPlayer.seekTo(positionMs.coerceAtLeast(0L))
        refreshPosition()
        // Forced rather than left to the interval: a seek is the clearest
        // possible statement of where the viewer wants to be, and losing it
        // means resuming somewhere they explicitly left.
        forceReport()
    }

    override fun seekBy(deltaMs: Long) {
        val duration = exoPlayer.duration
        val target = (exoPlayer.currentPosition + deltaMs).coerceAtLeast(0L)
        seekTo(if (duration > 0L) target.coerceAtMost(duration) else target)
    }

    override fun setSpeed(speed: Float) {
        exoPlayer.setPlaybackSpeed(speed)
        mutableState.update { it.copy(speed = speed) }
    }

    override fun selectAudioTrack(id: String) {
        applyOverride(C.TRACK_TYPE_AUDIO, id)
    }

    override fun selectSubtitleTrack(id: String?) {
        if (id == null) {
            exoPlayer.trackSelectionParameters =
                exoPlayer.trackSelectionParameters
                    .buildUpon()
                    .clearOverridesOfType(C.TRACK_TYPE_TEXT)
                    .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                    .build()
            publishTracks(exoPlayer.currentTracks)
            return
        }
        exoPlayer.trackSelectionParameters =
            exoPlayer.trackSelectionParameters
                .buildUpon()
                .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
                .build()
        applyOverride(C.TRACK_TYPE_TEXT, id)
    }

    override suspend fun switchSource(fileId: String) {
        val request = mutableState.value.request ?: return
        // Read the position before tearing anything down: after `prepare` the
        // player has been reset and `currentPosition` is zero, so reading it
        // afterwards would silently restart the title.
        val resumeAtMs = exoPlayer.currentPosition
        val wasPlaying = exoPlayer.playWhenReady
        val switched =
            request.copy(
                fileId = fileId,
                startPositionSecs = resumeAtMs / ProgressReporter.MILLIS_PER_SECOND,
            )
        if (prepare(switched, switched.startPositionSecs)) {
            exoPlayer.playWhenReady = wasPlaying
        }
    }

    override fun stop() {
        reporter.stop()
        forceReport()
        positionTicker?.cancel()
        positionTicker = null
        exoPlayer.stop()
        exoPlayer.clearMediaItems()
        currentFileId = null
        mutableState.value = PlaybackUiState()
    }

    override fun release() {
        reporter.stop()
        positionTicker?.cancel()
        positionTicker = null
        exoPlayer.removeListener(this)
        exoPlayer.release()
    }

    private suspend fun prepare(
        request: PlaybackRequest,
        startAtSecs: Double,
    ): Boolean {
        val config =
            try {
                repository.playbackConfig(request.fileId)
            } catch (failure: BeamException) {
                mutableState.update {
                    it.copy(request = request, error = failure.toFailure().asPlaybackFailure())
                }
                return false
            }

        val item =
            MediaItem
                .Builder()
                .setUri(config.url)
                .setMediaId(request.fileId)
                .setMediaMetadata(
                    MediaMetadata
                        .Builder()
                        .setTitle(request.title)
                        .setSubtitle(request.subtitle)
                        .setArtworkUri(request.artworkUrl?.let(android.net.Uri::parse))
                        .build(),
                ).build()

        val factory =
            beamDataSourceFactory(
                context,
                clients,
                config.headers,
                config.trustedFingerprints,
            )
        val source =
            androidx.media3.exoplayer.source
                .DefaultMediaSourceFactory(factory)
                .createMediaSource(item)

        exoPlayer.setMediaSource(source)
        exoPlayer.prepare()
        if (startAtSecs > 0.0) {
            exoPlayer.seekTo((startAtSecs * ProgressReporter.MILLIS_PER_SECOND).toLong())
        }

        currentFileId = request.fileId
        mutableState.value =
            PlaybackUiState(
                request = request,
                isBuffering = true,
                durationMs =
                    request.durationSecs
                        ?.let { (it * ProgressReporter.MILLIS_PER_SECOND).toLong() }
                        ?: PlaybackUiState.UNKNOWN_DURATION,
                positionMs = (startAtSecs * ProgressReporter.MILLIS_PER_SECOND).toLong(),
            )
        reporter.start(request.fileId, ::sample)
        startPositionTicker()
        return true
    }

    private fun applyOverride(
        trackType: Int,
        id: String,
    ) {
        val tracks = exoPlayer.currentTracks
        val group =
            tracks.groups.firstOrNull { candidate ->
                candidate.type == trackType &&
                    (0 until candidate.length).any { index ->
                        trackId(candidate, index) == id
                    }
            } ?: return
        val index = (0 until group.length).first { trackId(group, it) == id }

        exoPlayer.trackSelectionParameters =
            exoPlayer.trackSelectionParameters
                .buildUpon()
                .setOverrideForType(TrackSelectionOverride(group.mediaTrackGroup, index))
                .build()
        publishTracks(exoPlayer.currentTracks)
    }

    private fun startPositionTicker() {
        positionTicker?.cancel()
        positionTicker =
            scope.launch {
                while (isActive) {
                    refreshPosition()
                    delay(POSITION_POLL_MS)
                }
            }
    }

    private fun refreshPosition() {
        mutableState.update {
            it.copy(
                positionMs = exoPlayer.currentPosition.coerceAtLeast(0L),
                bufferedPositionMs = exoPlayer.bufferedPosition.coerceAtLeast(0L),
                durationMs =
                    exoPlayer.duration.takeIf { value -> value != C.TIME_UNSET }
                        ?: it.durationMs,
            )
        }
    }

    private fun sample(): PlaybackPosition? {
        val fileId = currentFileId ?: return null
        if (fileId.isEmpty()) return null
        return PlaybackPosition(
            positionMs = exoPlayer.currentPosition.coerceAtLeast(0L),
            durationMs = exoPlayer.duration.takeIf { it != C.TIME_UNSET },
        )
    }

    private fun forceReport() {
        val fileId = currentFileId ?: return
        val sample = sample() ?: return
        reporter.forceReport(fileId, sample)
    }

    // --- Player.Listener -------------------------------------------------

    override fun onIsPlayingChanged(isPlaying: Boolean) {
        mutableState.update { it.copy(isPlaying = isPlaying) }
    }

    override fun onPlaybackStateChanged(playbackState: Int) {
        mutableState.update {
            it.copy(
                isBuffering = playbackState == Player.STATE_BUFFERING,
                hasEnded = playbackState == Player.STATE_ENDED,
            )
        }
        if (playbackState == Player.STATE_READY) {
            refreshPosition()
        }
        if (playbackState == Player.STATE_ENDED) {
            // A finished title is reported at its real position rather than
            // its duration: the server decides what counts as watched, and
            // rounding up here would take that decision away from it.
            reporter.stop()
            forceReport()
        }
    }

    override fun onTracksChanged(tracks: Tracks) {
        publishTracks(tracks)
    }

    override fun onPlayerError(error: PlaybackException) {
        reporter.stop()
        forceReport()
        mutableState.update { it.copy(error = error.asPlaybackFailure(), isBuffering = false) }
    }

    private fun publishTracks(tracks: Tracks) {
        mutableState.update {
            it.copy(
                audioTracks = tracks.optionsOfType(C.TRACK_TYPE_AUDIO),
                subtitleTracks = tracks.optionsOfType(C.TRACK_TYPE_TEXT),
            )
        }
    }

    internal companion object {
        /**
         * Fast enough that the scrubber does not visibly step, slow enough not
         * to recompose the player chrome on every frame.
         */
        const val POSITION_POLL_MS: Long = 500L
    }
}

/** Stable identity for one track within a group. */
internal fun trackId(
    group: Tracks.Group,
    index: Int,
): String {
    val format = group.getTrackFormat(index)
    // `Format.id` is absent often enough that it cannot be the identity on its
    // own; the group and index disambiguate the rest.
    return format.id ?: "${group.type}:$index"
}

private fun Tracks.optionsOfType(trackType: Int): List<TrackOption> =
    groups.filter { it.type == trackType }.flatMap { group ->
        (0 until group.length).map { index ->
            val format = group.getTrackFormat(index)
            TrackOption(
                id = trackId(group, index),
                label =
                    format.label
                        ?: format.language
                        ?: "Track ${index + 1}",
                language = format.language,
                isSelected = group.isTrackSelected(index),
                isSupported = group.isTrackSupported(index),
            )
        }
    }

private fun BeamFailure.asPlaybackFailure(): PlaybackFailure =
    PlaybackFailure(message = message, isRetryable = retryable)

private fun PlaybackException.asPlaybackFailure(): PlaybackFailure {
    // Direct play means there is no server-side fallback for a file this
    // device cannot decode (ADR-0004), so a decoder failure is offered as
    // "try another source" rather than as "retry", which would fail the same
    // way every time.
    val decoderProblem = errorCode in DECODER_ERRORS
    return PlaybackFailure(
        message =
            when {
                decoderProblem -> {
                    "This device cannot decode this file."
                }

                errorCode == PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED -> {
                    "Lost the connection to the server."
                }

                errorCode == PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS -> {
                    "The server refused to send this file."
                }

                else -> {
                    localizedMessage ?: "Playback stopped unexpectedly."
                }
            },
        isRetryable = !decoderProblem,
        suggestsAnotherSource = decoderProblem,
    )
}

private val DECODER_ERRORS =
    setOf(
        PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
        PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
        PlaybackException.ERROR_CODE_DECODING_FAILED,
        PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
        PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
    )
