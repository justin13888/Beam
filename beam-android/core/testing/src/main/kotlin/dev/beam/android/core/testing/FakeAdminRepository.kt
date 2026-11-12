package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.AdminRepository
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ScanJobState
import uniffi.beam_client_core.ServerHealth

/**
 * An administrative surface that answers from memory.
 *
 * Libraries are listed through the catalog, not here, so a [catalog] passed
 * in is the server both share: deleting a library through this one takes it
 * out of that one's list, as the server would.
 */
public class FakeAdminRepository(
    private val catalog: FakeCatalogRepository? = null,
) : AdminRepository {
    /** Set to make every call throw, typically `Forbidden`. */
    public var failWith: BeamException? = null

    /** The dashboard snapshot. */
    public var statusValue: AdminStatus = Fixtures.adminStatus()

    /**
     * Accounts, held as mutable state rather than a fixed page.
     *
     * Stateful because the interesting assertion is that blocking an account
     * is *reflected* afterwards -- a fake that only records the call cannot
     * tell a working implementation from one that drops the write.
     */
    public var userList: MutableList<AdminUser> =
        mutableListOf(
            Fixtures.adminUser(),
            Fixtures.adminUser(id = "user-2", displayName = "Grace Hopper", isAdmin = true),
        )

    /**
     * Set to make [users] alone fail.
     *
     * Distinct from [failWith] because the user list is one section of the
     * dashboard: it failing must be distinguishable from the whole screen
     * failing.
     */
    public var usersFailWith: BeamException? = null

    /** Log lines returned by [logs]. */
    public var logEntries: List<AdminLogEntry> = emptyList()

    /** Events returned by [events]. */
    public var eventEntries: List<AdminEvent> = emptyList()

    /** Every (userId, disabled) pair passed to [setUserDisabled]. */
    public val disableCalls: MutableList<Pair<String, Boolean>> = mutableListOf()

    /** Every library id passed to [scanLibrary]. */
    public val scanCalls: MutableList<String> = mutableListOf()

    override suspend fun status(): AdminStatus {
        failWith?.let { throw it }
        return statusValue
    }

    override suspend fun health(): ServerHealth {
        failWith?.let { throw it }
        return ServerHealth("ok", "0.1.0", 86_400uL, "ok")
    }

    override suspend fun users(
        first: UInt?,
        after: String?,
    ): AdminUserPage {
        usersFailWith?.let { throw it }
        failWith?.let { throw it }
        return AdminUserPage(userList, userList.size.toULong(), endCursor = null, hasNextPage = false)
    }

    override suspend fun setUserDisabled(
        userId: String,
        disabled: Boolean,
    ) {
        failWith?.let { throw it }
        disableCalls += userId to disabled
        val index = userList.indexOfFirst { it.id == userId }
        if (index >= 0) {
            userList[index] = userList[index].copy(disabled = disabled)
        }
    }

    override suspend fun logs(
        first: UInt?,
        after: String?,
    ): AdminLogPage {
        failWith?.let { throw it }
        return AdminLogPage(logEntries, logEntries.size.toULong(), endCursor = null, hasNextPage = false)
    }

    override suspend fun events(limit: UInt?): List<AdminEvent> {
        failWith?.let { throw it }
        return eventEntries
    }

    override suspend fun createLibrary(
        name: String,
        rootPath: String,
    ): LibrarySummary {
        failWith?.let { throw it }
        return Fixtures.library(name = name)
    }

    override suspend fun deleteLibrary(libraryId: String) {
        failWith?.let { throw it }
        catalog?.let { shared -> shared.libraryList = shared.libraryList.filterNot { it.id == libraryId } }
    }

    override suspend fun scanLibrary(libraryId: String) {
        failWith?.let { throw it }
        scanCalls += libraryId
        // As the server does: an accepted scan is the library's latest job,
        // queued until its turn.
        scanJobs[libraryId] = Fixtures.scanJob(libraryId = libraryId, state = ScanJobState.QUEUED)
    }

    /** The latest scan of each library, keyed by library id; absent means none since start. */
    public val scanJobs: MutableMap<String, ScanJob> = mutableMapOf()

    /** Set to make [scanJob] alone fail, like [usersFailWith]. */
    public var scanJobFailWith: BeamException? = null

    /** How many times [scanJob] was asked, across every library. */
    public var scanJobReads: Int = 0
        private set

    override suspend fun scanJob(libraryId: String): ScanJob? {
        scanJobReads++
        scanJobFailWith?.let { throw it }
        failWith?.let { throw it }
        return scanJobs[libraryId]
    }

    override suspend fun refreshMetadata(mediaId: String) {
        failWith?.let { throw it }
    }

    /** What [playbackTelemetry] answers. */
    public var telemetry: PlaybackTelemetrySummary = Fixtures.playbackTelemetry()

    /** Set to make [playbackTelemetry] alone fail, like [usersFailWith]. */
    public var telemetryFailWith: BeamException? = null

    /** Every `(from, to)` range [playbackTelemetry] was asked for, in order. */
    public val telemetryRanges: MutableList<Pair<String?, String?>> = mutableListOf()

    override suspend fun playbackTelemetry(
        from: String?,
        to: String?,
    ): PlaybackTelemetrySummary {
        telemetryRanges += from to to
        telemetryFailWith?.let { throw it }
        failWith?.let { throw it }
        return telemetry.copy(from = from ?: telemetry.from, to = to ?: telemetry.to)
    }
}
