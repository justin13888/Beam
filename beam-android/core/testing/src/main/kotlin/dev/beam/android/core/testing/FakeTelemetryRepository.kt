package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.TelemetryRepository
import uniffi.beam_client_core.StartFailureCause
import uniffi.beam_client_core.SwitchTrigger
import uniffi.beam_client_core.TelemetryClientKind
import uniffi.beam_client_core.TelemetryFlush

/**
 * Playback telemetry held in memory, as the events the player recorded.
 *
 * Stateful in the way the core's queue is: [flush] empties what was recorded
 * into [flushedEvents], and [disabled] makes every later record a no-op, the
 * way the core behaves once a server answers that telemetry is off.
 */
public class FakeTelemetryRepository : TelemetryRepository {
    /** The kind last set. */
    public var clientKind: TelemetryClientKind? = null
        private set

    /** What is waiting, oldest first. */
    public val pendingEvents: MutableList<TelemetryEvent> = mutableListOf()

    /** What every flush so far took. */
    public val flushedEvents: MutableList<TelemetryEvent> = mutableListOf()

    /** How many times [flush] was called. */
    public var flushCount: Int = 0
        private set

    /** Whether the server has said no; every record after it is discarded. */
    public var disabled: Boolean = false

    override fun setClientKind(kind: TelemetryClientKind) {
        clientKind = kind
    }

    override fun recordStart(
        fileId: String,
        audioTrackIndex: UInt?,
    ): UInt = record(TelemetryEvent.Start(fileId, audioTrackIndex))

    override fun recordStartFailure(
        fileId: String,
        cause: StartFailureCause,
        audioTrackIndex: UInt?,
    ): UInt = record(TelemetryEvent.StartFailure(fileId, cause, audioTrackIndex))

    override fun recordRebuffer(
        fileId: String,
        durationMs: ULong,
    ): UInt = record(TelemetryEvent.Rebuffer(fileId, durationMs))

    override fun recordSourceSwitch(
        fromFileId: String,
        toFileId: String,
        trigger: SwitchTrigger,
    ): UInt = record(TelemetryEvent.SourceSwitch(fromFileId, toFileId, trigger))

    override suspend fun flush(): TelemetryFlush {
        flushCount++
        val sent = pendingEvents.size.toUInt()
        flushedEvents += pendingEvents
        pendingEvents.clear()
        return TelemetryFlush(sent = sent, dropped = 0u, pending = 0u, disabled = disabled)
    }

    /** Everything recorded, flushed or not, oldest first. */
    public val allEvents: List<TelemetryEvent> get() = flushedEvents + pendingEvents

    private fun record(event: TelemetryEvent): UInt {
        if (disabled) return 0u
        pendingEvents += event
        return pendingEvents.size.toUInt()
    }
}

/** One event captured by [FakeTelemetryRepository]. */
public sealed interface TelemetryEvent {
    /** A first frame. */
    public data class Start(
        val fileId: String,
        val audioTrackIndex: UInt?,
    ) : TelemetryEvent

    /** A failure before the first frame. */
    public data class StartFailure(
        val fileId: String,
        val cause: StartFailureCause,
        val audioTrackIndex: UInt?,
    ) : TelemetryEvent

    /** A mid-stream stall. */
    public data class Rebuffer(
        val fileId: String,
        val durationMs: ULong,
    ) : TelemetryEvent

    /** A move to another file of the same title. */
    public data class SourceSwitch(
        val fromFileId: String,
        val toFileId: String,
        val trigger: SwitchTrigger,
    ) : TelemetryEvent
}
