package dev.beam.android.core.media.player

import dev.beam.android.core.ffi.repository.PlaybackRepository
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Keeps the server's idea of where the viewer is roughly current.
 *
 * The throttle itself lives in the Rust core, shared with every other client,
 * so this deliberately does *not* decide when to send. It samples on a timer
 * and forces on the events where losing the position would be noticed, and the
 * core decides what actually goes over the wire.
 *
 * Forcing matters more than the interval. A viewer who pauses and closes the
 * app expects to resume where they stopped; if the last sample went out on the
 * interval, they lose up to a full interval of progress, which reads as the
 * app having forgotten. Pause, seek and stop are therefore always forced.
 */
internal class ProgressReporter(
    private val repository: PlaybackRepository,
    private val scope: CoroutineScope,
    private val sampleIntervalMs: Long = DEFAULT_SAMPLE_INTERVAL_MS,
) {
    private var ticker: Job? = null

    /** Begin sampling [position] until [stop] is called. */
    fun start(
        fileId: String,
        position: () -> PlaybackPosition?,
    ) {
        stop()
        ticker =
            scope.launch {
                while (isActive) {
                    delay(sampleIntervalMs)
                    val sample = position() ?: continue
                    report(fileId, sample, force = false)
                }
            }
    }

    /** Send a position now, bypassing the interval. */
    fun forceReport(
        fileId: String,
        sample: PlaybackPosition,
    ) {
        scope.launch { report(fileId, sample, force = true) }
    }

    /** Stop sampling. Does not send a final position; the caller forces that. */
    fun stop() {
        ticker?.cancel()
        ticker = null
    }

    private suspend fun report(
        fileId: String,
        sample: PlaybackPosition,
        force: Boolean,
    ) {
        // Failures are swallowed on purpose. The core queues an undeliverable
        // position durably and drains it later, so surfacing an error here
        // would interrupt playback for something already handled -- and there
        // is nothing the viewer could usefully do about it mid-title.
        runCatching {
            repository.reportProgress(
                fileId = fileId,
                positionSecs = sample.positionMs / MILLIS_PER_SECOND,
                durationSecs =
                    sample.durationMs
                        ?.takeIf { it > 0L }
                        ?.let { it / MILLIS_PER_SECOND },
                force = force,
            )
        }
    }

    internal companion object {
        /**
         * Matches `beam-web`'s `usePlaybackBeacon`, so a viewer moving between
         * the web app and the phone sees the same resume granularity rather
         * than two different answers to "where was I".
         */
        const val DEFAULT_SAMPLE_INTERVAL_MS: Long = 15_000L
        const val MILLIS_PER_SECOND: Double = 1_000.0
    }
}

/** A position sample, in milliseconds. */
internal data class PlaybackPosition(
    val positionMs: Long,
    val durationMs: Long?,
)
