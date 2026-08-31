import BeamFFI
import BeamModel
import Foundation

// The catalogue's playback records carry identifiers and a hydrated
// `MediaSummary`/`EpisodeSummary` rather than flattened display fields, since
// the same record backs a card, a row and a lock screen. Deriving the display
// text here rather than in each view is what stops "S2E4 - Title" appearing
// three ways in three places.

extension ContinueWatchingEntry {
    /// What to put on the first line.
    public var displayTitle: String {
        media?.title ?? episode?.title ?? "Untitled"
    }

    /// What to put on the second line: the episode for a show, the year
    /// otherwise.
    public var displaySubtitle: String? {
        if let episode {
            return "Episode \(episode.episodeNumber) - \(episode.title)"
        }
        return media?.year.map(String.init)
    }

    /// The widest artwork available, preferring a still from the title itself.
    public var artworkURL: String? {
        media?.backdropUrl ?? episode?.thumbnailUrl ?? media?.posterUrl
    }

    /// How far through, in `0...1`.
    ///
    /// Falls back to position over duration when the server sent no fraction,
    /// so a resume bar is drawn whenever it can be rather than only when the
    /// server did the division.
    public var fraction: Double {
        if let progressFraction { return min(max(progressFraction, 0), 1) }
        guard let durationSecs, durationSecs > 0 else { return 0 }
        return min(max(positionSecs / durationSecs, 0), 1)
    }

    /// A request that resumes this entry where it was left.
    public var playbackRequest: PlaybackRequest {
        PlaybackRequest(
            fileId: fileId,
            mediaId: mediaId,
            episodeId: episodeId,
            title: displayTitle,
            subtitle: displaySubtitle,
            artworkUrl: artworkURL,
            startPositionSeconds: positionSecs
        )
    }
}

extension HistoryEntry {
    /// What to put on the first line.
    public var displayTitle: String {
        media?.title ?? episode?.title ?? "Untitled"
    }

    /// What to put on the second line.
    public var displaySubtitle: String? {
        if let episode {
            return "Episode \(episode.episodeNumber) - \(episode.title)"
        }
        return media?.year.map(String.init)
    }

    /// Poster-shaped artwork, since history is a list rather than a shelf.
    public var artworkURL: String? {
        media?.posterUrl ?? episode?.thumbnailUrl
    }

    /// How far through, in `0...1`.
    public var fraction: Double {
        if let progressFraction { return min(max(progressFraction, 0), 1) }
        guard let durationSecs, durationSecs > 0 else { return 0 }
        return min(max(positionSecs / durationSecs, 0), 1)
    }

    /// A request that resumes this entry, or restarts it when it was finished.
    ///
    /// Restarting a completed title is the behaviour people expect from a
    /// history list: they are there to watch it again, not to see the credits.
    public var playbackRequest: PlaybackRequest {
        PlaybackRequest(
            fileId: fileId,
            mediaId: mediaId,
            episodeId: episodeId,
            title: displayTitle,
            subtitle: displaySubtitle,
            artworkUrl: artworkURL,
            startPositionSeconds: completed ? 0 : positionSecs
        )
    }
}

extension MediaSummary {
    /// The year, or nothing.
    public var displaySubtitle: String? {
        year.map(String.init)
    }
}
