package dev.beam.android.core.media.player

import androidx.media3.common.PlaybackException
import dev.beam.android.core.ffi.BeamFailure

internal fun BeamFailure.asPlaybackFailure(): PlaybackFailure =
    PlaybackFailure(message = message, isRetryable = retryable)

internal fun PlaybackException.asPlaybackFailure(): PlaybackFailure {
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
internal val NETWORK_ERRORS =
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
