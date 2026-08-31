import Foundation

/// Everything the player needs, assembled before it is presented.
///
/// Built by the detail and home screens rather than by the player, so the
/// player never has to reach back into a catalogue to find out what it is
/// showing -- which is what makes it presentable from a widget, a continue-
/// watching row, or a deep link with equal ease.
public struct PlaybackRequest: Equatable, Hashable, Sendable {
    /// The file to play.
    public let fileId: String
    /// The title this file belongs to, where the caller knows it.
    public let mediaId: String?
    /// The episode, for a show.
    public let episodeId: String?
    /// What to show in the player chrome and on the lock screen.
    public let title: String
    /// A second line: the show and episode number, or the year.
    public let subtitle: String?
    /// Artwork for the lock screen and AirPlay, where there is any.
    public let artworkUrl: String?
    /// Where to resume from.
    public let startPositionSeconds: Double

    /// Memberwise.
    public init(
        fileId: String,
        mediaId: String? = nil,
        episodeId: String? = nil,
        title: String,
        subtitle: String? = nil,
        artworkUrl: String? = nil,
        startPositionSeconds: Double = 0
    ) {
        self.fileId = fileId
        self.mediaId = mediaId
        self.episodeId = episodeId
        self.title = title
        self.subtitle = subtitle
        self.artworkUrl = artworkUrl
        self.startPositionSeconds = startPositionSeconds
    }
}
