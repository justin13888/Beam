package dev.beam.android.core.media.player

import androidx.media3.common.PlaybackException
import androidx.media3.datasource.HttpDataSource
import dev.beam.android.core.ffi.BeamFailure

internal fun BeamFailure.asPlaybackFailure(): PlaybackFailure =
    PlaybackFailure(message = message, isRetryable = retryable, requiresSignIn = requiresSignIn)

internal fun PlaybackException.asPlaybackFailure(): PlaybackFailure {
    // Direct play means there is no server-side fallback for a file this
    // device cannot decode (ADR-0004), so a decoder failure is offered as
    // "try another source" rather than as "retry", which would fail the same
    // way every time.
    val decoderProblem = errorCode in DECODER_ERRORS
    // Media3 collapses every refusal into one error code; the status is what
    // tells a session the server no longer honours from a file this viewer
    // may no longer see, and both from a server that is merely failing.
    val status = if (errorCode == PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS) httpStatus() else null
    val sessionRefused = status == HTTP_UNAUTHORIZED
    val accessLost = status in ACCESS_LOST_STATUSES
    return PlaybackFailure(
        message =
            when {
                decoderProblem -> {
                    "This device cannot decode this file."
                }

                errorCode == PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED -> {
                    "Lost the connection to the server."
                }

                // Worded as the API's own expiry is (`BeamErrors.kt`): it is the
                // same event, reached through the player's own connection
                // rather than through the core.
                sessionRefused -> {
                    "Your session expired. Sign in again to continue."
                }

                accessLost -> {
                    "This file is no longer available to you."
                }

                errorCode == PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS -> {
                    SERVER_REFUSED_MESSAGE
                }

                else -> {
                    localizedMessage ?: "Playback stopped unexpectedly."
                }
            },
        // Neither a refused session nor a withdrawn file answers differently
        // to the same request sent again.
        isRetryable = !decoderProblem && !sessionRefused && !accessLost,
        suggestsAnotherSource = decoderProblem,
        requiresSignIn = sessionRefused,
    )
}

/**
 * A stream refused with `401` while the core found the session still
 * honoured: the refusal came from something in front of the server, such as
 * a proxy, so it is the generic refusal a retry may get past, not an expiry.
 */
internal fun streamRefusedWithSessionHonoured(): PlaybackFailure =
    PlaybackFailure(message = SERVER_REFUSED_MESSAGE, isRetryable = true)

private const val SERVER_REFUSED_MESSAGE = "The server refused to send this file."

/**
 * The HTTP status behind this failure, if a server answered with one.
 *
 * Searched down the cause chain rather than read off the direct cause alone,
 * so a loader that wraps the data source's exception once more does not turn
 * a 401 back into a generic refusal. Bounded, because nothing guarantees a
 * cause chain is acyclic.
 */
private fun PlaybackException.httpStatus(): Int? =
    generateSequence(cause) { it.cause }
        .take(MAX_CAUSE_DEPTH)
        .filterIsInstance<HttpDataSource.InvalidResponseCodeException>()
        .firstOrNull()
        ?.responseCode

private const val MAX_CAUSE_DEPTH = 8

private const val HTTP_UNAUTHORIZED = 401

/**
 * The statuses that mean this viewer can no longer reach this file: refused
 * it outright, or no longer served it. The session was accepted, so signing in
 * again would not help.
 */
private val ACCESS_LOST_STATUSES = setOf(403, 404)

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
