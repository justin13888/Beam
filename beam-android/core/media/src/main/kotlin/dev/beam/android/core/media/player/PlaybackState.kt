package dev.beam.android.core.media.player

import dev.beam.android.core.model.PlaybackRequest

/** What the player is doing, as the UI needs to see it. */
public data class PlaybackUiState(
    /** What is loaded, or null before anything has been prepared. */
    val request: PlaybackRequest? = null,
    /** Whether the player intends to play, regardless of buffering. */
    val isPlaying: Boolean = false,
    /** Whether playback is stalled waiting for data. */
    val isBuffering: Boolean = false,
    /** Whether the current item has finished. */
    val hasEnded: Boolean = false,
    /** Current position in milliseconds. */
    val positionMs: Long = 0L,
    /** Duration in milliseconds, or [UNKNOWN_DURATION] before it is known. */
    val durationMs: Long = UNKNOWN_DURATION,
    /** How far the buffer reaches, in milliseconds. */
    val bufferedPositionMs: Long = 0L,
    /** Playback speed, where 1.0 is normal. */
    val speed: Float = 1f,
    /** Selectable audio tracks. */
    val audioTracks: List<TrackOption> = emptyList(),
    /** Selectable subtitle tracks. Always carries an explicit "off". */
    val subtitleTracks: List<TrackOption> = emptyList(),
    /** Why playback stopped, phrased for a person. */
    val error: PlaybackFailure? = null,
) {
    /**
     * Fraction watched, or null while the duration is unknown.
     *
     * Null rather than zero: a scrubber that renders a confident 0% for an
     * unknown duration is worse than one that renders as indeterminate.
     */
    public val progress: Float?
        get() =
            if (durationMs > 0L) {
                (positionMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
            } else {
                null
            }

    /** Whether anything is loaded at all. */
    public val isIdle: Boolean get() = request == null

    public companion object {
        /** Media3's sentinel for "not yet known". */
        public const val UNKNOWN_DURATION: Long = -1L
    }
}

/** One selectable audio or subtitle track. */
public data class TrackOption(
    /** Stable identity within the current item. */
    val id: String,
    /** What to show in the picker. */
    val label: String,
    /** BCP-47 language tag, where the file declares one. */
    val language: String? = null,
    /** Whether this track is currently selected. */
    val isSelected: Boolean = false,
    /**
     * Whether the device can actually decode this track.
     *
     * A track the device cannot decode is still listed, greyed, rather than
     * hidden: a viewer looking for a commentary track that silently vanished
     * has no way to tell a missing track from an unplayable one.
     */
    val isSupported: Boolean = true,
)

/** Why playback stopped. */
public data class PlaybackFailure(
    /** What to show the viewer. */
    val message: String,
    /** Whether retrying the same file could plausibly work. */
    val isRetryable: Boolean,
    /**
     * Whether the failure looks like a decoder rejecting the file.
     *
     * Direct play means an unsupported file has no server-side fallback
     * ([ADR-0004]), so the honest response is to offer another source rather
     * than to retry the same bytes and fail identically.
     */
    val suggestsAnotherSource: Boolean = false,
)
