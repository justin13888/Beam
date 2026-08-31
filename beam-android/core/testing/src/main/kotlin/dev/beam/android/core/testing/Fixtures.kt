package dev.beam.android.core.testing

import uniffi.beam_client_core.AdminCounts
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AudioTrackView
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.EnrichmentCounts
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.HistoryEntry
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MediaPage
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.MediaSummary
import uniffi.beam_client_core.SeasonSummary
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
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
        )

    public fun season(
        number: UInt = 1u,
        episodes: List<EpisodeSummary> = listOf(episode()),
    ): SeasonSummary =
        SeasonSummary(
            seasonNumber = number,
            posterUrl = null,
            episodeRuntimeMinutes = 52u,
            genres = listOf("Thriller"),
            episodes = episodes,
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
}
