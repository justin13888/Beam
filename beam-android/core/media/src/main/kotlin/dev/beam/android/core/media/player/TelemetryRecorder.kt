package dev.beam.android.core.media.player

import dev.beam.android.core.ffi.repository.TelemetryRepository
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import uniffi.beam_client_core.StartFailureCause
import uniffi.beam_client_core.SwitchTrigger

/**
 * Turns what the player saw into the core's playback telemetry events.
 *
 * Owns the one rule that is the player's rather than the core's: which phase
 * of a file's life an observation belongs to. Until the first frame of a file
 * is on screen, a failure is a failure *to start* and a stall is part of
 * starting; after it, a stall is a rebuffer and a failure is not a start
 * failure at all. Everything else -- the vocabulary, the batching, what the
 * server's answer means -- is the core's, so this only reports.
 *
 * Every call is fire-and-forget. Telemetry is never worth interrupting a
 * viewer for, so a failure to record or to flush is swallowed: the core keeps
 * what it could not send, and drops what the server refused.
 */
internal class TelemetryRecorder(
    private val telemetry: TelemetryRepository,
    private val scope: CoroutineScope,
) {
    /** The file being played, or null between files. */
    private var fileId: String? = null

    /** Whether [fileId]'s first frame has been reported. */
    private var hasStarted = false

    /** Whether [fileId] has already failed, so one attempt reports once. */
    private var hasFailed = false

    /** A different file is about to be prepared. */
    fun begin(fileId: String) {
        this.fileId = fileId
        hasStarted = false
        hasFailed = false
    }

    /** The current file's first frame is on screen. Reported once per file. */
    fun firstFrame(audioTrackIndex: UInt?) {
        val file = fileId ?: return
        if (hasStarted || hasFailed) return
        hasStarted = true
        record { telemetry.recordStart(file, audioTrackIndex) }
    }

    /**
     * The player failed. Reported only before the first frame: a failure
     * mid-film is not a failure to start, and the server has no other kind.
     */
    fun failed(
        isNetwork: Boolean,
        audioTrackIndex: UInt?,
    ) {
        val file = fileId ?: return
        if (hasStarted || hasFailed) return
        hasFailed = true
        val cause = if (isNetwork) StartFailureCause.Network else StartFailureCause.Other
        record { telemetry.recordStartFailure(file, cause, audioTrackIndex) }
    }

    /**
     * A stall of [durationMs] ended. Only one after the first frame is a
     * rebuffer; the player already leaves out the buffering a prepare or a
     * seek causes.
     */
    fun stalled(durationMs: Long) {
        val file = fileId ?: return
        if (!hasStarted || durationMs <= 0L) return
        record { telemetry.recordRebuffer(file, durationMs.toULong()) }
    }

    /** Playback is moving from [from] to [to], as [trigger] chose. */
    fun switched(
        from: String,
        to: String,
        trigger: SwitchTrigger,
    ) {
        record { telemetry.recordSourceSwitch(from, to, trigger) }
    }

    /** Playback ended, for now: whatever is waiting goes. */
    fun flush() {
        scope.launch { runCatching { telemetry.flush() } }
    }

    /** Playback stopped: forget the file, and send what is waiting. */
    fun end() {
        fileId = null
        flush()
    }

    private inline fun record(event: () -> UInt) {
        val pending = runCatching(event).getOrDefault(0u)
        // A request's worth waiting is sent now rather than at the next pause,
        // which on a long film may be hours away.
        if (pending >= FLUSH_AT) flush()
    }

    internal companion object {
        /** The most events the server takes in one request. */
        const val FLUSH_AT: UInt = 50u
    }
}
