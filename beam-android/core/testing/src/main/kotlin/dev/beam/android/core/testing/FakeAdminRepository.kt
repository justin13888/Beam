package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.AdminRepository
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.LibraryTelemetryPreview
import uniffi.beam_client_core.MatchCandidate
import uniffi.beam_client_core.MediaEnrichment
import uniffi.beam_client_core.MediaEnrichmentPage
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.PinSource
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
        val (items, endCursor, hasNextPage) = page(logEntries, first, after)
        return AdminLogPage(items, logEntries.size.toULong(), endCursor, hasNextPage)
    }

    override suspend fun events(limit: UInt?): List<AdminEvent> {
        failWith?.let { throw it }
        return limit?.let { eventEntries.take(it.toInt()) } ?: eventEntries
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

    /**
     * Every title's enrichment, newest change first. Stateful, like [userList]:
     * a fix-match or a lock is asserted through the row it changed.
     */
    public val enrichments: MutableList<MediaEnrichment> = mutableListOf()

    /** Set to make the matching calls -- search, fix, clear, locks -- alone fail. */
    public var matchingFailWith: BeamException? = null

    /** What [matchCandidates] answers, whatever the search. */
    public var candidates: List<MatchCandidate> = listOf(AdminFixtures.candidate())

    /** Every `(mediaId, query, year)` [matchCandidates] was asked, in order. */
    public val candidateSearches: MutableList<Triple<String, String?, UInt?>> = mutableListOf()

    /** What [libraryTelemetryPreview] answers. */
    public var libraryPreview: LibraryTelemetryPreview = AdminFixtures.libraryTelemetryPreview()

    override suspend fun enrichment(
        status: EnrichmentStatus?,
        first: UInt?,
        after: String?,
    ): MediaEnrichmentPage {
        failWith?.let { throw it }
        val admitted = enrichments.filter { status == null || it.status == status }
        val (items, endCursor, hasNextPage) = page(admitted, first, after)
        return MediaEnrichmentPage(items, admitted.size.toULong(), endCursor, hasNextPage)
    }

    override suspend fun mediaEnrichment(mediaId: String): MediaEnrichment {
        failWith?.let { throw it }
        return enrichments.firstOrNull { it.mediaId == mediaId }
            ?: throw BeamException.NotFound("Title not found", "about:blank")
    }

    override suspend fun matchCandidates(
        mediaId: String,
        query: String?,
        year: UInt?,
    ): List<MatchCandidate> {
        candidateSearches += Triple(mediaId, query, year)
        matchingFailWith?.let { throw it }
        failWith?.let { throw it }
        return candidates
    }

    // As the server does: the administrator's pin replaces the old match, and
    // the title is queued to be fetched by it.
    override suspend fun fixMatch(
        mediaId: String,
        externalRef: String,
    ): MediaEnrichment =
        change(mediaId) {
            it.copy(
                status = EnrichmentStatus.PENDING,
                pinnedRef = externalRef,
                pinSource = PinSource.ADMIN,
                matchedRef = null,
                matchConfidence = null,
            )
        }

    // As the server does: only an administrator's pin is cleared; an NFO's is the NFO's.
    override suspend fun clearMatch(mediaId: String) {
        change(mediaId) {
            if (it.pinSource != PinSource.ADMIN) {
                it
            } else {
                it.copy(status = EnrichmentStatus.PENDING, pinnedRef = null, pinSource = null)
            }
        }
    }

    override suspend fun setFieldLocks(
        mediaId: String,
        fields: List<MetadataField>,
    ): MediaEnrichment = change(mediaId) { it.copy(lockedFields = fields) }

    override suspend fun libraryTelemetryPreview(): LibraryTelemetryPreview {
        failWith?.let { throw it }
        return libraryPreview
    }

    /** Replace one title's row with [transform] of it, as a matching call does. */
    private fun change(
        mediaId: String,
        transform: (MediaEnrichment) -> MediaEnrichment,
    ): MediaEnrichment {
        matchingFailWith?.let { throw it }
        failWith?.let { throw it }
        val index = enrichments.indexOfFirst { it.mediaId == mediaId }
        if (index < 0) throw BeamException.NotFound("Title not found", "about:blank")
        return transform(enrichments[index]).also { enrichments[index] = it }
    }

    /** [first] of [all] after the cursor [after], which is the index it stopped at. */
    private fun <T> page(
        all: List<T>,
        first: UInt?,
        after: String?,
    ): Triple<List<T>, String?, Boolean> {
        val start = after?.toInt() ?: 0
        val end = first?.let { minOf(all.size, start + it.toInt()) } ?: all.size
        val items = all.subList(minOf(start, all.size), end)
        return Triple(items, end.toString().takeIf { items.isNotEmpty() }, end < all.size)
    }
}
