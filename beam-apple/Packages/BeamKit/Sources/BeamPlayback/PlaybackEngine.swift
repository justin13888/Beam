import BeamFFI
import BeamModel
import Foundation
import SwiftUI

/// What the player is doing.
public enum PlaybackStatus: Equatable, Sendable {
    /// Nothing loaded.
    case idle
    /// Opening the source and preparing decoders.
    case loading
    /// Playing.
    case playing
    /// Paused by the user or by an interruption.
    case paused
    /// Playing was requested but the buffer is empty.
    case buffering
    /// Reached the end of the item.
    case ended
    /// Stopped, with a reason worth showing.
    case failed(String)
}

/// One selectable audio or subtitle track.
public struct PlaybackTrack: Identifiable, Equatable, Sendable {
    /// Stable within one loaded item.
    public let id: String
    /// What to show in the menu.
    public let label: String
    /// ISO 639 code, where the container declared one.
    public let languageCode: String?
    /// Whether the container marks this as the default for its kind.
    public let isDefault: Bool
    /// Whether this client can actually render it.
    ///
    /// Present rather than filtered out, for the reason `capability::select`
    /// returns rejected sources with their reason: under direct play, "this
    /// audio track will never play here" is a permanent fact the viewer may
    /// want to act on by choosing a different source.
    public let isPlayable: Bool

    /// Memberwise.
    public init(
        id: String,
        label: String,
        languageCode: String? = nil,
        isDefault: Bool = false,
        isPlayable: Bool = true
    ) {
        self.id = id
        self.label = label
        self.languageCode = languageCode
        self.isDefault = isDefault
        self.isPlayable = isPlayable
    }
}

/// Everything a view needs to draw the player, in one value.
///
/// A single snapshot rather than a dozen observable properties, so a view
/// cannot render a half-applied state -- a position from after a seek beside a
/// duration from before the load.
public struct PlaybackSnapshot: Equatable, Sendable {
    /// What the player is doing.
    public var status: PlaybackStatus = .idle
    /// Where it is, in seconds.
    public var position: Double = 0
    /// How long the item is, where that is known.
    public var duration: Double?
    /// How much is buffered ahead, in seconds.
    public var bufferedAhead: Double = 0
    /// Selectable audio tracks.
    public var audioTracks: [PlaybackTrack] = []
    /// Selectable subtitle tracks.
    public var subtitleTracks: [PlaybackTrack] = []
    /// The audio track in use.
    public var selectedAudioTrackID: String?
    /// The subtitle track in use, or `nil` for none.
    public var selectedSubtitleTrackID: String?
    /// The subtitle text to display right now, where this engine renders
    /// subtitles itself rather than leaving them to the platform.
    public var activeSubtitleText: String?
    /// Whether seeking is possible.
    public var isSeekable: Bool = true

    /// An empty snapshot.
    public init() {}
}

/// Where the bytes are, and how to reach them.
public struct PlaybackItem: Equatable, Sendable {
    /// Absolute URL, or a `file://` URL for a completed download.
    public let url: URL
    /// Headers to attach, including the session cookie. Never a query token
    /// (FR-504).
    public let headers: [String: String]
    /// Certificates the user has accepted for this host, as whole-certificate
    /// SHA-256 digests.
    public let trustedFingerprints: [String]
    /// The host those fingerprints apply to.
    public let pinnedHost: String
    /// The container, lowercased, where the catalogue knows it.
    public let container: String?
    /// Where to start.
    public let startPositionSeconds: Double
    /// What to show in system playback UI.
    public let request: PlaybackRequest

    /// Memberwise.
    public init(
        url: URL,
        headers: [String: String] = [:],
        trustedFingerprints: [String] = [],
        pinnedHost: String = "",
        container: String? = nil,
        startPositionSeconds: Double = 0,
        request: PlaybackRequest
    ) {
        self.url = url
        self.headers = headers
        self.trustedFingerprints = trustedFingerprints
        self.pinnedHost = pinnedHost
        self.container = container
        self.startPositionSeconds = startPositionSeconds
        self.request = request
    }

    /// Build an item from what the core hands over for a file.
    public static func from(
        config: PlaybackHttpConfig,
        container: String?,
        request: PlaybackRequest
    ) -> PlaybackItem? {
        guard let url = URL(string: config.url) else { return nil }
        return PlaybackItem(
            url: url,
            headers: config.headers,
            trustedFingerprints: config.trustedFingerprints,
            pinnedHost: config.pinnedHost,
            container: container?.lowercased(),
            startPositionSeconds: request.startPositionSeconds,
            request: request
        )
    }
}

/// The seam between the player screen and whichever engine is driving it.
///
/// Beam owns the player shell and swaps the engine underneath, because neither
/// engine can do the whole job. `AVPlayerEngine` is preferred wherever
/// AVFoundation can open the container: it brings Picture in Picture, AirPlay,
/// the system transport UI, and years of buffering and seeking work that would
/// be foolish to reimplement. `SampleBufferEngine` exists only because
/// AVFoundation cannot open Matroska at all and the server will never remux it
/// (ADR-0004), so for those files we demux in the core and drive VideoToolbox
/// ourselves.
///
/// The protocol is what makes the player screen testable: `FakePlaybackEngine`
/// in `BeamTesting` conforms to it, so the view model can be driven with no
/// decoder at all -- the same discipline `PlayerTest.kt` applies by stubbing
/// `PlayerProvider.exoPlayer()` to a failure.
@MainActor
public protocol PlaybackEngine: AnyObject {
    /// The current state.
    var snapshot: PlaybackSnapshot { get }

    /// Called whenever the snapshot changes.
    ///
    /// A callback rather than an `AsyncStream` or a Combine publisher: the
    /// consumer is an `@Observable` view model that wants to store the latest
    /// value on the main actor, and a stream would add a task to cancel and a
    /// buffering policy to get wrong for no gain.
    var onSnapshotChange: (@MainActor (PlaybackSnapshot) -> Void)? { get set }

    /// The view that renders this engine's video output.
    func makeVideoView() -> AnyView

    /// Open `item` and prepare to play from its start position.
    func load(_ item: PlaybackItem) async throws

    /// Begin or resume.
    func play()

    /// Pause, keeping position.
    func pause()

    /// Move to `seconds`, clamped to the item.
    func seek(to seconds: Double) async

    /// Choose an audio track by ``PlaybackTrack/id``.
    func selectAudioTrack(id: String)

    /// Choose a subtitle track, or `nil` to turn them off.
    func selectSubtitleTrack(id: String?)

    /// Tear down decoders and release the source.
    func stop()
}
