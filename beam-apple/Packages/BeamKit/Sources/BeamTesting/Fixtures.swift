import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Builders for the core's records.
///
/// Builders rather than struct literals scattered through the tests, for the
/// reason `beam-domain`'s test-utils exist: a generated record gains a field
/// and every literal in the suite stops compiling, where a builder gains one
/// default. Mirrors `core/testing/Fixtures.kt`.
public enum Fixtures {
    /// A movie in the catalogue.
    public static func movie(
        id: String = "movie-1",
        title: String = "The Third Man",
        year: UInt32? = 1949,
        posterUrl: String? = "https://example.invalid/poster.jpg"
    ) -> MediaSummary {
        MediaSummary(
            id: id,
            kind: .movie,
            title: title,
            originalTitle: title,
            year: year,
            description: "A writer arrives in postwar Vienna.",
            posterUrl: posterUrl,
            backdropUrl: "https://example.invalid/backdrop.jpg",
            genres: ["Film-Noir", "Mystery"],
            runtimeMinutes: 104,
            tmdbRating: 82,
            fileId: "file-\(id)",
            seasonCount: 0,
            episodeCount: 0
        )
    }

    /// A show in the catalogue.
    public static func show(
        id: String = "show-1",
        title: String = "Pole to Pole",
        seasonCount: UInt32 = 2,
        episodeCount: UInt32 = 16
    ) -> MediaSummary {
        MediaSummary(
            id: id,
            kind: .show,
            title: title,
            originalTitle: title,
            year: 1992,
            description: "A journey down the meridian.",
            posterUrl: "https://example.invalid/show.jpg",
            backdropUrl: nil,
            genres: ["Documentary"],
            runtimeMinutes: nil,
            tmdbRating: 88,
            fileId: nil,
            seasonCount: seasonCount,
            episodeCount: episodeCount
        )
    }

    /// One page of catalogue results.
    public static func page(_ items: [MediaSummary], hasNextPage: Bool = false) -> MediaPage {
        MediaPage(
            items: items,
            endCursor: items.last?.id,
            hasNextPage: hasNextPage
        )
    }

    /// A playable source.
    public static func source(
        fileId: String = "file-1",
        container: String? = "mkv",
        videoCodec: String? = "h264",
        width: UInt32? = 1920,
        height: UInt32? = 1080,
        sizeBytes: UInt64 = 4_000_000_000
    ) -> MediaSourceView {
        MediaSourceView(
            fileId: fileId,
            sizeBytes: sizeBytes,
            durationSecs: 6240,
            container: container,
            mimeType: "video/x-matroska",
            videoCodec: videoCodec,
            width: width,
            height: height,
            bitRate: 8_000_000,
            hdrFormat: nil,
            audioTracks: [
                AudioTrackView(codec: "aac", language: "eng", channels: 6, isDefault: true)
            ],
            streamUrl: "https://beam.invalid/v1/files/\(fileId)/stream",
            downloadUrl: "https://beam.invalid/v1/files/\(fileId)/download"
        )
    }

    /// A selection that resolved to hardware decoding.
    public static func selection(
        source: MediaSourceView? = nil,
        rejected: [RejectedSource] = []
    ) -> SourceSelection {
        SourceSelection(
            source: source ?? Self.source(),
            playability: .hardware,
            audioTrackIndex: 0,
            reason: "Hardware decoding",
            rejected: rejected
        )
    }

    /// A source the device cannot play, with the reason.
    public static func rejection(
        fileId: String = "file-2",
        reason: RejectionReason = .videoCodecUnsupported
    ) -> RejectedSource {
        RejectedSource(
            fileId: fileId,
            reason: reason,
            detail: "This device cannot decode VC-1"
        )
    }

    /// A part-watched title.
    public static func continueWatching(
        mediaId: String = "movie-1",
        positionSecs: Double = 1200,
        durationSecs: Double? = 6240
    ) -> ContinueWatchingEntry {
        ContinueWatchingEntry(
            mediaId: mediaId,
            episodeId: nil,
            fileId: "file-\(mediaId)",
            kind: .movie,
            positionSecs: positionSecs,
            durationSecs: durationSecs,
            progressFraction: durationSecs.map { positionSecs / $0 },
            updatedAtUnix: 1_700_000_000,
            media: movie(id: mediaId),
            episode: nil
        )
    }

    /// A watched title.
    public static func historyEntry(
        mediaId: String = "movie-1",
        completed: Bool = false
    ) -> HistoryEntry {
        HistoryEntry(
            mediaId: mediaId,
            episodeId: nil,
            fileId: "file-\(mediaId)",
            kind: .movie,
            positionSecs: completed ? 6240 : 900,
            durationSecs: 6240,
            progressFraction: completed ? 1 : 900 / 6240,
            completed: completed,
            updatedAtUnix: 1_700_000_000,
            media: movie(id: mediaId),
            episode: nil
        )
    }

    /// A library.
    public static func library(
        id: String = "library-1",
        name: String = "Films",
        size: UInt32 = 412
    ) -> LibrarySummary {
        LibrarySummary(
            id: id,
            name: name,
            description: "Everything on the NAS",
            size: size,
            lastScanFileCount: size,
            lastScanStartedAtUnix: 1_700_000_000,
            lastScanFinishedAtUnix: 1_700_000_600
        )
    }

    /// A registered server, signed in.
    public static func server(
        id: String = "server-1",
        isActive: Bool = true
    ) -> ServerSummary {
        ServerSummary(
            id: id,
            displayName: "Home",
            baseUrl: "https://beam.invalid",
            state: .authenticated(
                user: UserSummary(
                    id: "user-1",
                    displayName: "Viewer",
                    email: "viewer@example.invalid",
                    isAdmin: false,
                    avatarUrl: nil
                )
            ),
            isActive: isActive
        )
    }

    /// The HTTP handoff for a file.
    public static func playbackConfig(fileId: String = "file-1") -> PlaybackHttpConfig {
        PlaybackHttpConfig(
            url: "https://beam.invalid/v1/files/\(fileId)/stream",
            headers: ["Cookie": "beam_session=opaque"],
            trustedFingerprints: [],
            pinnedHost: "beam.invalid"
        )
    }
}
