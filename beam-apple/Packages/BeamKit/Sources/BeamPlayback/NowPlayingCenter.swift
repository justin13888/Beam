import AVFoundation
import BeamModel
import Foundation
import MediaPlayer

/// The lock screen, Control Centre, and every remote that talks to them.
///
/// Wired up for both engines, not just `AVPlayerEngine`. `AVPlayer` populates
/// some of this itself, but `SampleBufferEngine` populates none of it, and a
/// player whose lock-screen controls work for MP4s and not for MKVs would be a
/// baffling difference to a viewer who has no idea what a container is.
@MainActor
public final class NowPlayingCenter {
    private var artworkTask: Task<Void, Never>?

    /// Handlers the transport controls invoke.
    public struct Commands {
        /// Resume.
        public var play: @MainActor () -> Void
        /// Pause.
        public var pause: @MainActor () -> Void
        /// Move to an absolute position.
        public var seek: @MainActor (Double) -> Void
        /// Skip forward by the standard interval.
        public var skipForward: @MainActor () -> Void
        /// Skip back by the standard interval.
        public var skipBackward: @MainActor () -> Void

        /// Memberwise.
        public init(
            play: @escaping @MainActor () -> Void,
            pause: @escaping @MainActor () -> Void,
            seek: @escaping @MainActor (Double) -> Void,
            skipForward: @escaping @MainActor () -> Void,
            skipBackward: @escaping @MainActor () -> Void
        ) {
            self.play = play
            self.pause = pause
            self.seek = seek
            self.skipForward = skipForward
            self.skipBackward = skipBackward
        }
    }

    /// How far the skip commands move, in seconds.
    public static let skipInterval: Double = 15

    /// Credentials for Beam's artwork endpoint, set by
    /// ``configureArtworkAccess(headers:trustedFingerprints:pinnedHost:)``.
    private var artworkHeaders: [String: String] = [:]
    private var artworkTrust: TrustingSessionDelegate?
    private var artworkSession: URLSession?

    /// A centre with nothing playing.
    public init() {}

    /// Configure the audio session for playback and take the remote commands.
    ///
    /// The category is what allows audio to continue with the screen locked and
    /// what makes the app the "now playing" app. Without it playback stops the
    /// moment the screen turns off, which reads as a crash.
    public func activate(commands: Commands) {
        #if os(iOS)
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .moviePlayback)
        try? session.setActive(true)
        #endif

        let center = MPRemoteCommandCenter.shared()
        center.playCommand.isEnabled = true
        center.playCommand.addTarget { _ in
            MainActor.assumeIsolated { commands.play() }
            return .success
        }
        center.pauseCommand.isEnabled = true
        center.pauseCommand.addTarget { _ in
            MainActor.assumeIsolated { commands.pause() }
            return .success
        }
        center.changePlaybackPositionCommand.isEnabled = true
        center.changePlaybackPositionCommand.addTarget { event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else {
                return .commandFailed
            }
            MainActor.assumeIsolated { commands.seek(event.positionTime) }
            return .success
        }
        center.skipForwardCommand.isEnabled = true
        center.skipForwardCommand.preferredIntervals = [NSNumber(value: Self.skipInterval)]
        center.skipForwardCommand.addTarget { _ in
            MainActor.assumeIsolated { commands.skipForward() }
            return .success
        }
        center.skipBackwardCommand.isEnabled = true
        center.skipBackwardCommand.preferredIntervals = [NSNumber(value: Self.skipInterval)]
        center.skipBackwardCommand.addTarget { _ in
            MainActor.assumeIsolated { commands.skipBackward() }
            return .success
        }
    }

    /// Publish what is playing and where it has got to.
    public func update(request: PlaybackRequest, snapshot: PlaybackSnapshot) {
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: request.title,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: snapshot.position,
            MPNowPlayingInfoPropertyPlaybackRate: snapshot.status == .playing ? 1.0 : 0.0,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.video.rawValue,
        ]
        if let subtitle = request.subtitle {
            info[MPMediaItemPropertyArtist] = subtitle
        }
        if let duration = snapshot.duration {
            info[MPMediaItemPropertyPlaybackDuration] = duration
        }

        // Preserve artwork already fetched: rebuilding the dictionary on every
        // tick would otherwise make the lock screen flicker between the poster
        // and nothing.
        let center = MPNowPlayingInfoCenter.default()
        if let existing = center.nowPlayingInfo?[MPMediaItemPropertyArtwork] {
            info[MPMediaItemPropertyArtwork] = existing
        }
        center.nowPlayingInfo = info

        loadArtworkIfNeeded(from: request.artworkUrl)
    }

    /// Clear the now-playing state and release the audio session.
    public func deactivate() {
        artworkTask?.cancel()
        artworkTask = nil
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
        MPRemoteCommandCenter.shared().playCommand.removeTarget(nil)
        MPRemoteCommandCenter.shared().pauseCommand.removeTarget(nil)
        MPRemoteCommandCenter.shared().changePlaybackPositionCommand.removeTarget(nil)
        MPRemoteCommandCenter.shared().skipForwardCommand.removeTarget(nil)
        MPRemoteCommandCenter.shared().skipBackwardCommand.removeTarget(nil)
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false)
        #endif
    }

    /// Tell the centre how to reach Beam for artwork.
    ///
    /// Beam serves poster art itself now (ADR-0015), so the lock screen's
    /// image is a first-party authenticated fetch rather than an anonymous CDN
    /// one: it needs the session cookie, and on a LAN server with a self-signed
    /// certificate it needs the trust decision the viewer already made. Both
    /// travel on the `PlaybackItem` already being played, so this is fed from
    /// there rather than reaching for a second source of the same facts.
    public func configureArtworkAccess(
        headers: [String: String],
        trustedFingerprints: [String],
        pinnedHost: String
    ) {
        artworkHeaders = headers
        artworkTrust = TrustingSessionDelegate(
            evaluator: CertificateTrustEvaluator(
                trustedFingerprints: trustedFingerprints,
                pinnedHost: pinnedHost
            )
        )
        artworkSession?.finishTasksAndInvalidate()
        artworkSession = nil
    }

    /// The session artwork is fetched over, built once per trust decision.
    private func artworkFetchSession() -> URLSession {
        if let artworkSession { return artworkSession }
        let created = URLSession(
            configuration: .default,
            delegate: artworkTrust,
            delegateQueue: nil
        )
        artworkSession = created
        return created
    }

    private func loadArtworkIfNeeded(from urlString: String?) {
        guard artworkTask == nil,
            let urlString,
            let url = URL(string: urlString)
        else {
            return
        }

        var request = URLRequest(url: url)
        for (field, value) in artworkHeaders {
            request.setValue(value, forHTTPHeaderField: field)
        }
        let session = artworkFetchSession()

        artworkTask = Task { [weak self] in
            guard let (data, _) = try? await session.data(for: request) else { return }
            guard !Task.isCancelled, self != nil else { return }
            await MainActor.run {
                guard let image = PlatformImage(data: data) else { return }
                let artwork = MPMediaItemArtwork(boundsSize: image.size) { _ in image }
                MPNowPlayingInfoCenter.default().nowPlayingInfo?[MPMediaItemPropertyArtwork] =
                    artwork
            }
        }
    }
}

#if canImport(UIKit)
import UIKit

/// The platform's image type, so the artwork path is written once.
typealias PlatformImage = UIImage
#else
import AppKit

/// The platform's image type, so the artwork path is written once.
typealias PlatformImage = NSImage
#endif
