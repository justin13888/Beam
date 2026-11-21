package dev.beam.android.core.ffi.repository

import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.LibraryTelemetryPreview
import uniffi.beam_client_core.MatchCandidate
import uniffi.beam_client_core.MediaEnrichment
import uniffi.beam_client_core.MediaEnrichmentPage
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ServerHealth
import javax.inject.Inject
import javax.inject.Singleton

/** The administrative surface, available only to administrators. */
public interface AdminRepository {
    /** The dashboard snapshot. */
    public suspend fun status(): AdminStatus

    /** The server's own health report. */
    public suspend fun health(): ServerHealth

    /** One page of user accounts: [first] of them, after the page whose `endCursor` is [after]. */
    public suspend fun users(
        first: UInt?,
        after: String?,
    ): AdminUserPage

    /** Block or unblock an account. */
    public suspend fun setUserDisabled(
        userId: String,
        disabled: Boolean,
    )

    /**
     * One page of the operational log, newest first: [first] lines, after the page whose
     * `endCursor` is [after]. Its `total` is how many lines the server holds.
     */
    public suspend fun logs(
        first: UInt?,
        after: String?,
    ): AdminLogPage

    /** Recent server events, newest first. */
    public suspend fun events(limit: UInt?): List<AdminEvent>

    /** Create a library from a path on the server. */
    public suspend fun createLibrary(
        name: String,
        rootPath: String,
    ): LibrarySummary

    /** Delete a library and everything indexed into it. */
    public suspend fun deleteLibrary(libraryId: String)

    /**
     * Start a rescan of a library. Returns once the server has accepted it; the
     * scan itself runs on the server afterwards.
     */
    public suspend fun scanLibrary(libraryId: String)

    /**
     * The latest scan of a library since the server started, running or
     * finished; null when it has not been scanned since.
     */
    public suspend fun scanJob(libraryId: String): ScanJob?

    /** Re-fetch metadata for one title. */
    public suspend fun refreshMetadata(mediaId: String)

    /**
     * Playback telemetry from [from] to [to], inclusive UTC days as
     * `YYYY-MM-DD`; null for the server's default of the last thirty days.
     */
    public suspend fun playbackTelemetry(
        from: String?,
        to: String?,
    ): PlaybackTelemetrySummary

    /**
     * One page of titles by enrichment status, newest change first: [first] of
     * them, after the page whose `endCursor` is [after]. A null [status] lists
     * every title; [EnrichmentStatus.UNMATCHED] is the unmatched list.
     */
    public suspend fun enrichment(
        status: EnrichmentStatus?,
        first: UInt?,
        after: String?,
    ): MediaEnrichmentPage

    /** Where one title's enrichment stands, its locked fields included. */
    public suspend fun mediaEnrichment(mediaId: String): MediaEnrichment

    /**
     * The providers' candidates to fix a title's match to, best first. A null
     * [query] or [year] searches by the title's own.
     */
    public suspend fun matchCandidates(
        mediaId: String,
        query: String?,
        year: UInt?,
    ): List<MatchCandidate>

    /** Fix a title's match to [externalRef], answering with the title as queued. */
    public suspend fun fixMatch(
        mediaId: String,
        externalRef: String,
    ): MediaEnrichment

    /** Clear an administrator's fixed match. */
    public suspend fun clearMatch(mediaId: String)

    /** Lock exactly [fields] of a title; every other field is unlocked. */
    public suspend fun setFieldLocks(
        mediaId: String,
        fields: List<MetadataField>,
    ): MediaEnrichment

    /** The anonymous library report the server would send now, and where. Sends nothing. */
    public suspend fun libraryTelemetryPreview(): LibraryTelemetryPreview
}

@Singleton
internal class BeamAdminRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : AdminRepository {
        override suspend fun status(): AdminStatus = client.adminStatus()

        override suspend fun health(): ServerHealth = client.health()

        override suspend fun users(
            first: UInt?,
            after: String?,
        ): AdminUserPage = client.adminUsers(first, after)

        override suspend fun setUserDisabled(
            userId: String,
            disabled: Boolean,
        ) = client.setUserDisabled(userId, disabled)

        override suspend fun logs(
            first: UInt?,
            after: String?,
        ): AdminLogPage = client.adminLogs(first, after)

        override suspend fun events(limit: UInt?): List<AdminEvent> = client.adminEvents(limit)

        override suspend fun createLibrary(
            name: String,
            rootPath: String,
        ): LibrarySummary = client.createLibrary(name, rootPath)

        override suspend fun deleteLibrary(libraryId: String) = client.deleteLibrary(libraryId)

        override suspend fun scanLibrary(libraryId: String) = client.scanLibrary(libraryId)

        override suspend fun scanJob(libraryId: String): ScanJob? = client.libraryScan(libraryId)

        override suspend fun refreshMetadata(mediaId: String) = client.refreshMediaMetadata(mediaId)

        override suspend fun playbackTelemetry(
            from: String?,
            to: String?,
        ): PlaybackTelemetrySummary = client.adminPlaybackTelemetry(from, to)

        override suspend fun enrichment(
            status: EnrichmentStatus?,
            first: UInt?,
            after: String?,
        ): MediaEnrichmentPage = client.adminEnrichment(status, first, after)

        override suspend fun mediaEnrichment(mediaId: String): MediaEnrichment = client.mediaEnrichment(mediaId)

        override suspend fun matchCandidates(
            mediaId: String,
            query: String?,
            year: UInt?,
        ): List<MatchCandidate> = client.matchCandidates(mediaId, query, year)

        override suspend fun fixMatch(
            mediaId: String,
            externalRef: String,
        ): MediaEnrichment = client.fixMediaMatch(mediaId, externalRef)

        override suspend fun clearMatch(mediaId: String) = client.clearMediaMatch(mediaId)

        override suspend fun setFieldLocks(
            mediaId: String,
            fields: List<MetadataField>,
        ): MediaEnrichment = client.setMediaFieldLocks(mediaId, fields)

        override suspend fun libraryTelemetryPreview(): LibraryTelemetryPreview = client.libraryTelemetryPreview()
    }
