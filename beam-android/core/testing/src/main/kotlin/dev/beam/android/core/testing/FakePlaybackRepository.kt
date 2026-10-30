package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.PlaybackRepository
import kotlinx.coroutines.CompletableDeferred
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.DeviceProfile
import uniffi.beam_client_core.HistoryPage
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.PlaybackHttpConfig
import uniffi.beam_client_core.PlaybackPlan
import uniffi.beam_client_core.ProgressOutcome
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.SourceSelection

/** A playback surface that records what it was told. */
public class FakePlaybackRepository : PlaybackRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    /** The profile last handed to [setDeviceProfile]. */
    public var deviceProfile: DeviceProfile? = null

    /** Sources returned by [sources]. */
    public var sourceList: List<MediaSourceView> = listOf(Fixtures.source())

    /** The selection returned by [selectSource]. */
    public var selection: SourceSelection? = null

    /** Rows returned by [continueWatching]. */
    public var continueWatchingRows: List<ContinueWatchingEntry> =
        listOf(Fixtures.continueWatching())

    /** The page returned by [history]. */
    public var historyPage: HistoryPage =
        HistoryPage(listOf(Fixtures.historyEntry()), 1uL, endCursor = "c1", hasNextPage = false)

    /** The `after` cursor of every [history] request, in order. */
    public val historyAfters: MutableList<String?> = mutableListOf()

    /**
     * Every progress report, in order.
     *
     * Records the duration and the force flag as well as the position, because
     * the interesting assertions are about *those*: whether a pause forced a
     * send, and whether an unknown duration was sent as absent rather than as
     * a confident zero.
     */
    public val reportedProgress: MutableList<ProgressReport> = mutableListOf()

    /** What [reportProgress] returns. */
    public var progressOutcome: ProgressOutcome = ProgressOutcome.Sent(0.0)

    /** How many positions are queued; [flushProgress] sends them all. */
    public var pending: UInt = 0u

    /** How many times [flushProgress] was called. */
    public var flushCalls: Int = 0

    /** Set while the server is unreachable: [flushProgress] sends nothing and keeps [pending]. */
    public var serverDown: Boolean = false

    override suspend fun setDeviceProfile(profile: DeviceProfile) {
        deviceProfile = profile
    }

    /** Every title [sources] was asked about, in order. */
    public val sourceRequests: MutableList<String> = mutableListOf()

    override suspend fun sources(mediaId: String): List<MediaSourceView> {
        sourceRequests += mediaId
        failWith?.let { throw it }
        return sourceList
    }

    /** The policy [selectSource] was last asked for. */
    public var selectionPolicy: QualityPolicy? = null
        private set

    override suspend fun selectSource(
        mediaId: String,
        policy: QualityPolicy,
    ): SourceSelection {
        selectionPolicy = policy
        failWith?.let { throw it }
        return selection ?: throw BeamException.NotFound("This title has no playable files", "about:blank")
    }

    /** The fallback returned by [downgradeSource]; null means nothing smaller plays. */
    public var downgrade: SourceSelection? = null

    /** Every `(titleId, currentFileId)` [downgradeSource] was asked for, in order. */
    public val downgradeRequests: MutableList<Pair<String, String>> = mutableListOf()

    /** When set, [downgradeSource] holds its answer until this completes. */
    public var downgradeHold: CompletableDeferred<Unit>? = null

    override suspend fun downgradeSource(
        titleId: String,
        currentFileId: String,
    ): SourceSelection? {
        downgradeRequests += titleId to currentFileId
        downgradeHold?.await()
        failWith?.let { throw it }
        return downgrade
    }

    override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig {
        failWith?.let { throw it }
        return PlaybackHttpConfig(
            url = "https://beam.test/v1/files/$fileId/stream",
            headers = mapOf("Cookie" to "beam_session=test"),
            trustedFingerprints = emptyList(),
            pinnedHost = "beam.test",
        )
    }

    /** Plans [playbackPlan] answers, keyed by the file asked for; any other file is planned alone. */
    public val plans: MutableMap<String, PlaybackPlan> = mutableMapOf()

    /** Every `(titleId, fileId)` [playbackPlan] was asked for, in order. */
    public val planRequests: MutableList<Pair<String?, String>> = mutableListOf()

    override suspend fun playbackPlan(
        titleId: String?,
        fileId: String,
    ): PlaybackPlan {
        planRequests += titleId to fileId
        failWith?.let { throw it }
        return plans[fileId] ?: Fixtures.plan(listOf(Fixtures.part(fileId)))
    }

    /**
     * The ids the viewer has marked watched and not since unmarked.
     *
     * A set rather than a call log, so marking then unmarking reads back as
     * unwatched -- what the server would answer.
     */
    public val watched: MutableSet<String> = mutableSetOf()

    override suspend fun markWatched(id: String) {
        failWith?.let { throw it }
        watched += id
    }

    override suspend fun markUnwatched(id: String) {
        failWith?.let { throw it }
        watched -= id
    }

    override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> {
        failWith?.let { throw it }
        return continueWatchingRows
    }

    override suspend fun history(
        first: UInt?,
        after: String?,
    ): HistoryPage {
        historyAfters += after
        failWith?.let { throw it }
        return historyPage
    }

    override suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome {
        reportedProgress += ProgressReport(fileId, positionSecs, durationSecs, force)
        failWith?.let { throw it }
        return progressOutcome
    }

    override suspend fun flushProgress(): UInt {
        flushCalls += 1
        failWith?.let { throw it }
        // The core does not throw for an unreachable server: it stops at the
        // first failed send, backs that entry off and answers what it sent.
        if (serverDown) return 0u
        val sent = pending
        pending = 0u
        return sent
    }

    override suspend fun pendingProgressCount(): UInt {
        failWith?.let { throw it }
        return pending
    }
}

/** One position report captured by [FakePlaybackRepository]. */
public data class ProgressReport(
    /** The file the position belongs to. */
    val fileId: String,
    /** Where the viewer had got to, in seconds. */
    val positionSecs: Double,
    /** The title's length, where the player knew it. */
    val durationSecs: Double?,
    /** Whether the core was asked to bypass its throttle. */
    val force: Boolean,
)
