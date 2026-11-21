package dev.beam.android.core.testing

import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.CountRange
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.EventCategory
import uniffi.beam_client_core.FileSizeCount
import uniffi.beam_client_core.FileSizeRange
import uniffi.beam_client_core.LibraryReport
import uniffi.beam_client_core.LibraryTelemetryPreview
import uniffi.beam_client_core.LogLevel
import uniffi.beam_client_core.MatchCandidate
import uniffi.beam_client_core.MediaEnrichment
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.NamedCount
import uniffi.beam_client_core.PinSource
import uniffi.beam_client_core.TotalSizeRange

/**
 * Builders for the admin area's pages: log lines, events, enrichment rows,
 * fix-match candidates and the library report.
 */
public object AdminFixtures {
    /** One operational log line. */
    public fun logEntry(
        id: String = "log-1",
        message: String = "Scan finished",
        level: LogLevel = LogLevel.INFO,
    ): AdminLogEntry =
        AdminLogEntry(
            id = id,
            level = level,
            category = "library_scan",
            message = message,
            details = null,
            createdAt = "2026-01-01T12:30:00Z",
        )

    /** One server event. */
    public fun event(
        id: String = "event-1",
        message: String = "Scan finished",
    ): AdminEvent =
        AdminEvent(
            id = id,
            level = LogLevel.INFO,
            category = EventCategory.LIBRARY_SCAN,
            message = message,
            libraryId = "films",
            libraryName = "Films",
            timestampUnix = 1_700_000_000L,
        )

    /** One title's enrichment, unmatched unless said otherwise. */
    public fun enrichment(
        mediaId: String = "title-1",
        title: String = "Dark",
        kind: MediaKind = MediaKind.SHOW,
        status: EnrichmentStatus = EnrichmentStatus.UNMATCHED,
        pinnedRef: String? = null,
        pinSource: PinSource? = null,
        lockedFields: List<MetadataField> = emptyList(),
        lastError: String? = "No candidate scored high enough.",
    ): MediaEnrichment =
        MediaEnrichment(
            mediaId = mediaId,
            kind = kind,
            title = title,
            year = 2017u,
            status = status,
            attemptCount = 1u,
            matchedRef = pinnedRef,
            matchConfidence = null,
            pinnedRef = pinnedRef,
            pinSource = pinSource,
            lockedFields = lockedFields,
            lastError = lastError,
            nextAttemptAtUnix = null,
            enrichedAtUnix = null,
            updatedAtUnix = 1_700_000_000L,
        )

    /** A candidate a provider offers for a fix-match. */
    public fun candidate(
        externalRef: String = "tmdb:70523",
        title: String = "Dark",
        score: Double = 0.9,
    ): MatchCandidate =
        MatchCandidate(
            externalRef = externalRef,
            title = title,
            originalTitle = null,
            year = 2017u,
            score = score,
        )

    /** A preview of the library report, sent to a configured collector. */
    public fun libraryTelemetryPreview(destinationConfigured: Boolean = true): LibraryTelemetryPreview =
        LibraryTelemetryPreview(
            destinationConfigured = destinationConfigured,
            destinationOrigin = "https://collector.example".takeIf { destinationConfigured },
            lastSentAtUnix = null,
            nextSendAtUnix = 1_700_086_400L.takeIf { destinationConfigured },
            report =
                LibraryReport(
                    schemaVersion = 1u,
                    serverVersion = "1.2.3",
                    generatedOn = "2026-01-01",
                    libraries = CountRange.FROM1_TO9,
                    movies = CountRange.FROM100_TO999,
                    shows = CountRange.FROM10_TO99,
                    seasons = CountRange.FROM10_TO99,
                    episodes = CountRange.FROM1000_TO9999,
                    movieFiles = CountRange.FROM100_TO999,
                    episodeFiles = CountRange.FROM1000_TO9999,
                    unclassifiedFiles = CountRange.NONE,
                    containers = listOf(NamedCount("matroska", CountRange.FROM1000_TO9999)),
                    videoCodecs = listOf(NamedCount("hevc", CountRange.FROM100_TO999)),
                    audioCodecs = listOf(NamedCount("aac", CountRange.AT_LEAST10000)),
                    subtitleCodecs = emptyList(),
                    fileSizes = listOf(FileSizeCount(FileSizeRange.UNDER1_GIB, CountRange.FROM1_TO9)),
                    totalSize = TotalSizeRange.FROM1_TO10_TIB,
                ),
            contentType = "application/json",
            payload = """{"resourceMetrics":[]}""",
        )
}
