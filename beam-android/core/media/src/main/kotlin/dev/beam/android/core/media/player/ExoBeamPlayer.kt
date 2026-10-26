// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.player

import android.content.Context
import android.content.pm.PackageManager
import androidx.core.net.toUri
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
import dev.beam.android.core.ffi.repository.TelemetryRepository
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
import uniffi.beam_client_core.PlaybackPart
import uniffi.beam_client_core.PlaybackPlan
import uniffi.beam_client_core.RebufferMonitor
import uniffi.beam_client_core.SwitchTrigger
import uniffi.beam_client_core.TelemetryClientKind

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
    telemetry: TelemetryRepository,
    private val clients: BeamHttpClientFactory,
    private val scope: CoroutineScope,
) : BeamPlayer,
    Player.Listener {
    private val mutableState = MutableStateFlow(PlaybackUiState())
    override val state: StateFlow<PlaybackUiState> = mutableState.asStateFlow()

    private val reporter = ProgressReporter(repository, scope)

    /** Starts, start failures, rebuffers and switches, for the server's counts. */
    private val recorder = TelemetryRecorder(telemetry, scope)

    /** When the open stall began, on [nowMs]'s clock. */
    private var stallStartedAtMs: ULong = 0uL
    private var positionTicker: Job? = null

    /** The file playing: the source's own, or the part of it the player is in. */
    private var currentFileId: String? = null

    /** Where the loaded source's parts sit end to end, for a switch to hold position. */
    private var timeline: PartTimeline? = null

    /**
     * Whether rebuffering on this file is sustained. The rule is the core's;
     * this side only reports the stalls, timed on the player's own monotonic
     * clock, so a test player built with a fake clock drives it too.
     */
    private val rebuffers = RebufferMonitor()

    /**
     * True from a prepare or a seek until playback is next ready. Buffering
     * then is the player filling an empty buffer on purpose, not the
     * connection falling behind, and is never reported as a stall.
     */
    private var isFillingOnPurpose = false

    /** Whether a reported stall is still open. */
    private var isStalled = false

    init {
        exoPlayer.addListener(this)
        // A television reports as one, so the operator's counts can tell a
        // living-room decoder from a phone's.
        val isTelevision = context.packageManager.hasSystemFeature(PackageManager.FEATURE_LEANBACK)
        runCatching {
            telemetry.setClientKind(
                if (isTelevision) TelemetryClientKind.ANDROID_TV else TelemetryClientKind.ANDROID,
            )
        }
    }

    override suspend fun play(request: PlaybackRequest) {
        // The resume position is within the file the request names, which for
        // a multi-part movie is the part the viewer was last in.
        val started =
            prepare(request) { plan ->
                PartPosition(
                    index = plan.startPartIndex.toInt(),
                    offsetMs = (request.startPositionSecs * ProgressReporter.MILLIS_PER_SECOND).toLong(),
                )
            }
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
        // A paused viewer is not waiting on the network.
        endStall()
        reporter.stop()
        forceReport()
        recorder.flush()
    }

    override fun seekTo(positionMs: Long) {
        endStall()
        isFillingOnPurpose = true
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

    override suspend fun switchSource(
        fileId: String,
        trigger: SwitchTrigger,
    ) {
        val request = mutableState.value.request ?: return
        recorder.switched(from = request.fileId, to = fileId, trigger = trigger)
        // Read the position before tearing anything down: after `prepare` the
        // player has been reset and `currentPosition` is zero, so reading it
        // afterwards would silently restart the title. Read as a moment of the
        // whole film, not of the part playing, so a switch between a
        // multi-part version and a whole file lands at the same scene.
        val resumeAtMs =
            timeline?.absoluteMs(exoPlayer.currentMediaItemIndex, exoPlayer.currentPosition)
                ?: exoPlayer.currentPosition
        val wasPlaying = exoPlayer.playWhenReady
        val switched =
            request.copy(
                fileId = fileId,
                startPositionSecs = resumeAtMs / ProgressReporter.MILLIS_PER_SECOND,
            )
        if (prepare(switched) { plan -> plan.timeline().locate(resumeAtMs) }) {
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
        timeline = null
        mutableState.value = PlaybackUiState()
        recorder.end()
    }

    override fun release() {
        reporter.stop()
        recorder.end()
        positionTicker?.cancel()
        positionTicker = null
        exoPlayer.removeListener(this)
        exoPlayer.release()
        rebuffers.close()
    }

    /**
     * Load the source [request] names, every part of it, and position the
     * player where [startAt] says within that plan.
     */
    private suspend fun prepare(
        request: PlaybackRequest,
        startAt: (PlaybackPlan) -> PartPosition,
    ): Boolean {
        // Sources belong to a movie or an episode; a show has none of its own,
        // so an episode's are asked for by the episode.
        val titleId = request.episodeId ?: request.mediaId.takeIf { it.isNotEmpty() }
        val plan =
            try {
                repository.playbackPlan(titleId, request.fileId)
            } catch (failure: BeamException) {
                mutableState.update {
                    it.copy(request = request, error = failure.toFailure().asPlaybackFailure())
                }
                return false
            }
        if (plan.parts.isEmpty()) return false

        val factory =
            androidx.media3.exoplayer.source.DefaultMediaSourceFactory(
                beamDataSourceFactory(
                    context,
                    clients,
                    plan.headers,
                    plan.trustedFingerprints,
                ),
            )
        // One item per part (issue #233): the player moves to the next as one
        // ends, and each is a direct play of its own file.
        val sources =
            plan.parts.mapIndexed { index, part ->
                factory.createMediaSource(mediaItem(request, part, index, plan.parts.size))
            }
        val start =
            startAt(plan).let { position ->
                position.copy(index = position.index.coerceIn(0, plan.parts.lastIndex))
            }
        val startPart = plan.parts[start.index]

        // A different file: whatever the last one's stalls said about it, they
        // say nothing about this one.
        rebuffers.reset()
        recorder.begin(request.fileId)
        isStalled = false
        isFillingOnPurpose = true
        // Set before the items, so the transition the new playlist reports is
        // recognised as this start rather than a move to the next part.
        currentFileId = startPart.fileId
        timeline = plan.timeline()
        exoPlayer.setMediaSources(sources, start.index, start.offsetMs.coerceAtLeast(0L))
        exoPlayer.prepare()

        mutableState.value =
            PlaybackUiState(
                request = request,
                isBuffering = true,
                durationMs =
                    (startPart.durationSecs ?: request.durationSecs.takeIf { plan.parts.size == 1 })
                        ?.let { (it * ProgressReporter.MILLIS_PER_SECOND).toLong() }
                        ?: PlaybackUiState.UNKNOWN_DURATION,
                positionMs = start.offsetMs.coerceAtLeast(0L),
                partIndex = start.index,
                partCount = plan.parts.size,
            )
        reporter.start(startPart.fileId, ::sample)
        startPositionTicker()
        return true
    }

    private fun mediaItem(
        request: PlaybackRequest,
        part: PlaybackPart,
        index: Int,
        count: Int,
    ): MediaItem {
        // The lock screen says which part is playing, so "Part 2 of 2" is not
        // mistaken for the film restarting.
        val subtitle =
            if (count > 1) {
                listOfNotNull(request.subtitle, "Part ${index + 1} of $count").joinToString(" · ")
            } else {
                request.subtitle
            }
        return MediaItem
            .Builder()
            .setUri(part.url)
            .setMediaId(part.fileId)
            .setSubtitleConfigurations(
                part.sidecarSubtitles.mapNotNull(::sidecarSubtitle).map { sidecar ->
                    MediaItem.SubtitleConfiguration
                        .Builder(sidecar.url.toUri())
                        .setId(sidecar.id)
                        .setMimeType(sidecar.mimeType)
                        .setLanguage(sidecar.language)
                        .setLabel(sidecar.label)
                        .setSelectionFlags(sidecar.selectionFlags)
                        .setRoleFlags(sidecar.roleFlags)
                        .build()
                },
            ).setMediaMetadata(
                MediaMetadata
                    .Builder()
                    .setTitle(request.title)
                    .setSubtitle(subtitle)
                    .setArtworkUri(request.artworkUrl?.let(android.net.Uri::parse))
                    .build(),
            ).build()
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
                    // Belt and braces for a seek the player satisfied without
                    // ever leaving READY: the flag must not outlive the fill it
                    // was set for, or every later stall would go unreported.
                    if (exoPlayer.playbackState == Player.STATE_READY) isFillingOnPurpose = false
                    // A stall that never ends produces no further player
                    // events, and it is the one that most needs to trip.
                    if (isStalled) publishRebuffering(rebuffers.poll(nowMs()))
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

    private fun nowMs(): ULong = exoPlayer.clock.elapsedRealtime().toULong()

    private fun startStall() {
        if (isStalled || isFillingOnPurpose || !exoPlayer.playWhenReady) return
        isStalled = true
        stallStartedAtMs = nowMs()
        publishRebuffering(rebuffers.stallStarted(stallStartedAtMs))
    }

    private fun endStall() {
        if (!isStalled) return
        isStalled = false
        val endedAtMs = nowMs()
        recorder.stalled(
            durationMs = if (endedAtMs > stallStartedAtMs) (endedAtMs - stallStartedAtMs).toLong() else 0L,
        )
        publishRebuffering(rebuffers.stallEnded(endedAtMs))
    }

    /**
     * Which of the file's audio tracks is playing, counted the way the server
     * lists the file's `audio_tracks`: the selected one's position among the
     * audio groups, which ExoPlayer exposes one per stream in container order,
     * the order probing records them in. Null before any is selected.
     */
    private fun selectedAudioTrackIndex(): UInt? =
        exoPlayer.currentTracks.groups
            .filter { it.type == C.TRACK_TYPE_AUDIO }
            .indexOfFirst { it.isSelected }
            .takeIf { it >= 0 }
            ?.toUInt()

    private fun publishRebuffering(sustained: Boolean) {
        if (sustained && !mutableState.value.isRebufferingSustained) {
            mutableState.update { it.copy(isRebufferingSustained = true) }
        }
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

    override fun onPlayWhenReadyChanged(
        playWhenReady: Boolean,
        reason: Int,
    ) {
        // The media session (notification, headset, audio focus) flips
        // playWhenReady on the player directly, bypassing pause() and
        // resume(), so the stall follows the flag here rather than there.
        if (!playWhenReady) {
            endStall()
        } else if (exoPlayer.playbackState == Player.STATE_BUFFERING) {
            startStall()
        }
    }

    override fun onPlaybackStateChanged(playbackState: Int) {
        mutableState.update {
            it.copy(
                isBuffering = playbackState == Player.STATE_BUFFERING,
                hasEnded = playbackState == Player.STATE_ENDED,
            )
        }
        when (playbackState) {
            Player.STATE_BUFFERING -> {
                startStall()
            }

            Player.STATE_READY -> {
                isFillingOnPurpose = false
                endStall()
            }

            else -> {
                endStall()
            }
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

    override fun onMediaItemTransition(
        mediaItem: MediaItem?,
        reason: Int,
    ) {
        val fileId = mediaItem?.mediaId ?: return
        if (fileId == currentFileId || currentFileId == null) return
        // The next part of the same source. Nothing is reported for the part
        // that ended -- only the last part can mark the title played, and that
        // is the server's call -- and from here progress is the new part's, at
        // the position within it, which is what the server records.
        reporter.stop()
        currentFileId = fileId
        recorder.partChanged(fileId)
        rebuffers.reset()
        isStalled = false
        val index = exoPlayer.currentMediaItemIndex
        mutableState.update {
            it.copy(
                partIndex = index,
                positionMs = 0L,
                durationMs = PlaybackUiState.UNKNOWN_DURATION,
                isRebufferingSustained = false,
            )
        }
        reporter.start(fileId, ::sample)
    }

    override fun onRenderedFirstFrame() {
        recorder.firstFrame(selectedAudioTrackIndex())
    }

    override fun onPlayerError(error: PlaybackException) {
        recorder.failed(
            isNetwork = error.errorCode in NETWORK_ERRORS,
            audioTrackIndex = selectedAudioTrackIndex(),
        )
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

/** Where this plan's parts sit end to end. */
internal fun PlaybackPlan.timeline(): PartTimeline =
    PartTimeline(parts.map { part -> part.durationSecs?.let { (it * ProgressReporter.MILLIS_PER_SECOND).toLong() } })

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
                        ?: format.language?.let(::languageName)
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

/**
 * The failures that are the connection rather than the file or the server: a
 * start failure under one of these is counted as `network`, anything else as
 * `other`. A bad HTTP status is the server answering, so it is not here.
 */
private val NETWORK_ERRORS =
    setOf(
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT,
    )

private val DECODER_ERRORS =
    setOf(
        PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
        PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
        PlaybackException.ERROR_CODE_DECODING_FAILED,
        PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
        PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
    )
