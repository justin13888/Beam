package dev.beam.android.core.testing

import uniffi.beam_client_core.AdminCounts
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.AudioTrackView
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.DeviceSession
import uniffi.beam_client_core.EnrichmentCounts
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.FailureRateRow
import uniffi.beam_client_core.FileContentType
import uniffi.beam_client_core.FileIndexStatus
import uniffi.beam_client_core.HistoryEntry
import uniffi.beam_client_core.LibraryFileSummary
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MediaPage
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.MediaSummary
import uniffi.beam_client_core.PlaybackPart
import uniffi.beam_client_core.PlaybackPlan
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.RebufferBucket
import uniffi.beam_client_core.RebufferBucketRow
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ScanJobState
import uniffi.beam_client_core.SeasonSummary
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
import uniffi.beam_client_core.SourceSwitchRow
import uniffi.beam_client_core.SubtitleOrigin
import uniffi.beam_client_core.SubtitleTrackView
import uniffi.beam_client_core.SwitchTrigger
import uniffi.beam_client_core.UserSummary

/**
 * Builders for the core's records.
 *
 * Every parameter has a default that produces a valid value, so a test names
 * only the field it is actually about. A test that reads
 * `movie(title = "Le Samourai")` says what it is testing; one that spells out
 * fourteen fields buries it.
 */
public object Fixtures {
    public fun movie(
        id: String = "movie-1",
        title: String = "Le Samourai",
        year: UInt? = 1967u,
        posterUrl: String? = "https://beam.test/poster.jpg",
        fileId: String? = "file-1",
        genres: List<String> = listOf("Crime", "Drama"),
        runtimeMinutes: UInt? = 105u,
        tmdbRating: UInt? = 81u,
        description: String? = "A contract killer plans his last job.",
        played: Boolean = false,
    ): MediaSummary =
        MediaSummary(
            id = id,
            kind = MediaKind.MOVIE,
            title = title,
            originalTitle = title,
            year = year,
            description = description,
            posterUrl = posterUrl,
            backdropUrl = null,
            genres = genres,
            runtimeMinutes = runtimeMinutes,
            tmdbRating = tmdbRating,
            fileId = fileId,
            seasonCount = 0u,
            episodeCount = 0u,
            played = played,
        )

    public fun show(
        id: String = "show-1",
        title: String = "Le Bureau",
        year: UInt? = 2015u,
        seasonCount: UInt = 2u,
        episodeCount: UInt = 12u,
        posterUrl: String? = "https://beam.test/show.jpg",
        genres: List<String> = listOf("Thriller"),
    ): MediaSummary =
        MediaSummary(
            id = id,
            kind = MediaKind.SHOW,
            title = title,
            originalTitle = title,
            year = year,
            description = "Undercover officers, handled from Paris.",
            posterUrl = posterUrl,
            backdropUrl = null,
            genres = genres,
            runtimeMinutes = 52u,
            tmdbRating = 88u,
            fileId = null,
            seasonCount = seasonCount,
            episodeCount = episodeCount,
        )

    public fun episode(
        id: String = "episode-1",
        number: UInt = 1u,
        title: String = "Pilot",
        fileId: String? = "file-1",
        durationSecs: Double? = 3_120.0,
        played: Boolean = false,
    ): EpisodeSummary =
        EpisodeSummary(
            id = id,
            episodeNumber = number,
            title = title,
            description = "The handler returns from six years under.",
            thumbnailUrl = "https://beam.test/still.jpg",
            airDate = "2015-05-05",
            durationSecs = durationSecs,
            fileId = fileId,
            played = played,
        )

    public fun season(
        number: UInt = 1u,
        episodes: List<EpisodeSummary> = listOf(episode()),
        id: String = "season-$number",
    ): SeasonSummary =
        SeasonSummary(
            seasonNumber = number,
            posterUrl = null,
            episodeRuntimeMinutes = 52u,
            genres = listOf("Thriller"),
            episodes = episodes,
            id = id,
            played = episodes.isNotEmpty() && episodes.all { it.played },
            watchedEpisodeCount = episodes.count { it.played }.toUInt(),
        )

    public fun movieDetail(summary: MediaSummary = movie()): MediaDetail = MediaDetail.Movie(summary)

    public fun showDetail(
        summary: MediaSummary = show(),
        seasons: List<SeasonSummary> = listOf(season()),
    ): MediaDetail = MediaDetail.Show(summary, seasons)

    public fun page(
        items: List<MediaSummary> = listOf(movie(), show()),
        endCursor: String? = null,
        hasNextPage: Boolean = false,
    ): MediaPage = MediaPage(items, endCursor, hasNextPage)

    public fun source(
        fileId: String = "file-1",
        container: String? = "mkv",
        videoCodec: String? = "hevc",
        width: UInt? = 3840u,
        height: UInt? = 2160u,
        bitRate: ULong? = 24_000_000uL,
        hdrFormat: String? = null,
        sizeBytes: ULong = 18_000_000_000uL,
        audioTracks: List<AudioTrackView> = listOf(audioTrack()),
    ): MediaSourceView =
        MediaSourceView(
            fileId = fileId,
            sizeBytes = sizeBytes,
            durationSecs = 6_300.0,
            container = container,
            mimeType = "video/x-matroska",
            videoCodec = videoCodec,
            width = width,
            height = height,
            bitRate = bitRate,
            hdrFormat = hdrFormat,
            audioTracks = audioTracks,
            streamUrl = "https://beam.test/v1/files/$fileId/stream",
            downloadUrl = "https://beam.test/v1/files/$fileId/download",
        )

    public fun audioTrack(
        codec: String = "eac3",
        language: String? = "eng",
        channels: UShort = 6u,
        isDefault: Boolean = true,
    ): AudioTrackView = AudioTrackView(codec, language, channels, isDefault)

    public fun continueWatching(
        mediaId: String = "movie-1",
        fileId: String = "file-1",
        positionSecs: Double = 1_800.0,
        durationSecs: Double? = 6_300.0,
        media: MediaSummary? = movie(),
        episode: EpisodeSummary? = null,
    ): ContinueWatchingEntry =
        ContinueWatchingEntry(
            mediaId = mediaId,
            episodeId = episode?.id,
            fileId = fileId,
            kind = if (episode == null) MediaKind.MOVIE else MediaKind.SHOW,
            positionSecs = positionSecs,
            durationSecs = durationSecs,
            progressFraction = durationSecs?.let { (positionSecs / it).coerceIn(0.0, 1.0) },
            updatedAtUnix = 1_767_225_600L,
            media = media,
            episode = episode,
        )

    public fun historyEntry(
        mediaId: String = "movie-1",
        completed: Boolean = true,
        media: MediaSummary? = movie(),
    ): HistoryEntry =
        HistoryEntry(
            mediaId = mediaId,
            episodeId = null,
            fileId = "file-1",
            kind = MediaKind.MOVIE,
            positionSecs = 6_300.0,
            durationSecs = 6_300.0,
            progressFraction = 1.0,
            completed = completed,
            updatedAtUnix = 1_767_225_600L,
            media = media,
            episode = null,
        )

    public fun library(
        id: String = "library-1",
        name: String = "Films",
        size: UInt = 412u,
        scanRunning: Boolean = false,
    ): LibrarySummary =
        LibrarySummary(
            id = id,
            name = name,
            description = "Everything on the NAS.",
            size = size,
            lastScanFileCount = 412u,
            lastScanStartedAtUnix = 1_767_225_600L,
            lastScanFinishedAtUnix = if (scanRunning) null else 1_767_225_900L,
        )

    /**
     * A known server. Signed in by default, which is also what the core
     * restores a stored session as: `Authenticated`, carrying the identity the
     * server confirmed when the session was stored.
     */
    public fun server(
        id: String = "https-beam-local-8000",
        displayName: String = "Home",
        isActive: Boolean = true,
        state: SessionState = SessionState.Authenticated(user()),
    ): ServerSummary =
        ServerSummary(
            id = id,
            displayName = displayName,
            baseUrl = "https://beam.local:8000",
            state = state,
            isActive = isActive,
        )

    /** A plan of [parts], starting in the one at [startIndex]. */
    public fun plan(
        parts: List<PlaybackPart> = listOf(part()),
        startIndex: UInt = 0u,
    ): PlaybackPlan =
        PlaybackPlan(
            parts = parts,
            startPartIndex = startIndex,
            headers = mapOf("Cookie" to "beam_session=test"),
            trustedFingerprints = emptyList(),
            pinnedHost = "beam.test",
        )

    /** One part of a [plan]. */
    public fun part(
        fileId: String = "file-1",
        durationSecs: Double? = 3_000.0,
        sidecars: List<SubtitleTrackView> = emptyList(),
    ): PlaybackPart =
        PlaybackPart(
            fileId = fileId,
            url = "https://beam.test/v1/files/$fileId/stream",
            durationSecs = durationSecs,
            sidecarSubtitles = sidecars,
        )

    /** A subtitle file beside a media file. */
    public fun sidecar(
        id: String = "sidecar-1",
        codec: String = "subrip",
        language: String? = "eng",
        title: String? = null,
        isForced: Boolean = false,
        isHearingImpaired: Boolean = false,
        webvtt: Boolean = false,
    ): SubtitleTrackView =
        SubtitleTrackView(
            origin = SubtitleOrigin.SIDECAR,
            index = null,
            sidecarId = id,
            codec = codec,
            language = language,
            title = title,
            isDefault = false,
            isForced = isForced,
            isHearingImpaired = isHearingImpaired,
            isText = true,
            url = "https://beam.test/v1/files/file-1/subtitles/$id",
            webvttUrl = if (webvtt) "https://beam.test/v1/files/file-1/subtitles/$id/webvtt" else null,
        )

    /** The latest scan of a library. */
    public fun scanJob(
        libraryId: String = "library-1",
        state: ScanJobState = ScanJobState.RUNNING,
        processed: ULong = 30uL,
        total: ULong? = 120uL,
        failure: String? = null,
    ): ScanJob =
        ScanJob(
            id = "scan-$libraryId",
            libraryId = libraryId,
            state = state,
            totalCount = total,
            processedCount = processed,
            addedCount = 12uL,
            changedCount = 3uL,
            failedCount = 0uL,
            missingCount = 1uL,
            finishedAtUnix =
                if (state == ScanJobState.SUCCEEDED || state == ScanJobState.FAILED) 1_767_225_900L else null,
            failure = failure,
        )

    public fun user(
        id: String = "user-1",
        displayName: String = "Ada",
        isAdmin: Boolean = true,
    ): UserSummary =
        UserSummary(
            id = id,
            displayName = displayName,
            email = "ada@beam.test",
            isAdmin = isAdmin,
            avatarUrl = null,
        )

    public fun adminStatus(): AdminStatus =
        AdminStatus(
            version = "0.1.0",
            uptimeSecs = 86_400uL,
            counts = AdminCounts(libraries = 3uL, files = 1_204uL, users = 4uL),
            enrichment =
                EnrichmentCounts(
                    pending = 12uL,
                    enriched = 1_180uL,
                    failed = 2uL,
                    unmatched = 10uL,
                ),
            recentScans = emptyList(),
        )

    /** One signed-in device. */
    public fun deviceSession(
        id: String = "session-1",
        ip: String = "192.168.1.24",
        lastActiveUnix: Long = 1_700_000_000L,
    ): DeviceSession =
        DeviceSession(
            id = id,
            deviceHash = "d41d8cd98f00b204",
            ip = ip,
            createdAtUnix = lastActiveUnix - 86_400L,
            lastActiveUnix = lastActiveUnix,
        )

    /** One user account, as the admin screen lists them. */
    public fun adminUser(
        id: String = "user-1",
        displayName: String = "Ada Lovelace",
        isAdmin: Boolean = false,
        disabled: Boolean = false,
    ): AdminUser =
        AdminUser(
            id = id,
            displayName = displayName,
            email = "$id@beam.test",
            avatarUrl = null,
            isAdmin = isAdmin,
            disabled = disabled,
            createdAtUnix = 1_690_000_000L,
        )

    /** One indexed file, as a library's contents list shows it. */
    public fun libraryFile(
        id: String = "file-1",
        libraryId: String = "library-1",
        path: String = "/media/films/Arrival (2016)/Arrival.mkv",
        status: FileIndexStatus = FileIndexStatus.KNOWN,
        contentType: FileContentType = FileContentType.MOVIE,
    ): LibraryFileSummary =
        LibraryFileSummary(
            id = id,
            libraryId = libraryId,
            path = path,
            sizeBytes = 4_500_000_000uL,
            containerFormat = "matroska",
            mimeType = "video/x-matroska",
            durationSecs = 6_960.0,
            contentType = contentType,
            status = status,
            scannedAtUnix = 1_700_000_000L,
        )

    /** A playback telemetry summary with one codec, one client and one switch. */
    public fun playbackTelemetry(
        enabled: Boolean = true,
        started: ULong = 9uL,
        failed: ULong = 1uL,
    ): PlaybackTelemetrySummary =
        PlaybackTelemetrySummary(
            enabled = enabled,
            from = "2026-09-24",
            to = "2026-10-23",
            retentionDays = 400u,
            started = started,
            failed = failed,
            rebuffers = 3uL,
            rebufferTotalMs = 4_500uL,
            sourceSwitches = 1uL,
            byVideoCodec = listOf(failureRate("hevc", started, failed)),
            byAudioCodec = listOf(failureRate("aac", started, failed)),
            byClient = listOf(failureRate("android", started, failed)),
            rebufferHistogram =
                listOf(
                    RebufferBucketRow(RebufferBucket.UNDER1_SECS, 1uL),
                    RebufferBucketRow(RebufferBucket.FROM1_TO3_SECS, 2uL),
                    RebufferBucketRow(RebufferBucket.FROM3_TO10_SECS, 0uL),
                    RebufferBucketRow(RebufferBucket.FROM10_TO30_SECS, 0uL),
                    RebufferBucketRow(RebufferBucket.AT_LEAST30_SECS, 0uL),
                ),
            switches =
                listOf(
                    SourceSwitchRow(
                        clientKind = "android",
                        trigger = SwitchTrigger.AUTO,
                        fromHeightClass = "uhd",
                        toHeightClass = "fhd",
                        count = 1uL,
                    ),
                ),
        )

    /** One failure-rate row, its rate derived from its counts. */
    public fun failureRate(
        label: String,
        started: ULong,
        failed: ULong,
    ): FailureRateRow {
        val attempts = started + failed
        return FailureRateRow(
            label = label,
            started = started,
            failed = failed,
            failureRate = if (attempts == 0uL) 0.0 else failed.toDouble() / attempts.toDouble(),
        )
    }
}
