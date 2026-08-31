import BeamCore
import BeamFFI
import BeamModel
import BeamPlayback
import Foundation
import SwiftUI

/// The state the whole app shares: who is signed in, what is playing, and
/// where each tab has navigated to.
///
/// One model rather than state scattered across screens, because the player is
/// presented from four of them and the downloads coordinator is read by three.
/// Mirrors `beam-android`'s `MainViewModel` plus the shell's own back stacks.
@MainActor
@Observable
public final class AppModel {
    /// Whether a signed-in server is selected.
    public private(set) var isSignedIn = false
    /// Whether the launch restore has finished.
    public private(set) var hasRestored = false
    /// The tab or sidebar item showing.
    public var selectedTab: TopLevelDestination = .home
    /// One navigation stack per tab, so switching tabs preserves each one's
    /// place -- which is what people expect and what a single shared stack
    /// cannot do.
    public var paths: [TopLevelDestination: NavigationPath] = [:]
    /// What the player is showing, if anything.
    public var player: PlayerPresentation?

    /// The service graph.
    public let services: ServiceContainer
    /// Offline downloads.
    public let downloads: DownloadCoordinator

    /// Build the shell over a service graph.
    public init(services: ServiceContainer) {
        self.services = services
        self.downloads = DownloadCoordinator(
            playback: services.playback,
            allowsCellular: services.currentPreferences.allowCellularDownloads
        )
    }

    /// Restore the registry and tell the core what this device can decode.
    ///
    /// The profile must be pushed before anything is browsed: the core refuses
    /// to select a source without one, so a screen that loaded first would
    /// report every title as unplayable.
    public func start() async {
        let servers = (try? await services.servers.restore()) ?? []
        isSignedIn = servers.contains { server in
            server.isActive && Self.isAuthenticated(server.state)
        }
        services.playback.setDeviceProfile(
            DeviceProfileFactory.make(
                allowSoftwareDecode: services.currentPreferences.allowSoftwareDecode
            )
        )
        hasRestored = true
    }

    /// Apply a preferences change everywhere it has an effect.
    ///
    /// The device profile is rebuilt because `allowSoftwareDecode` changes
    /// which sources the core will offer, and a preference that only took
    /// effect on the next launch would look broken.
    public func update(preferences: UserPreferences) {
        services.update(preferences)
        services.playback.setDeviceProfile(
            DeviceProfileFactory.make(allowSoftwareDecode: preferences.allowSoftwareDecode)
        )
        downloads.setAllowsCellular(preferences.allowCellularDownloads)
    }

    /// Note that sign-in has completed.
    public func signedIn() {
        isSignedIn = true
    }

    /// Note that the session has ended, and clear navigation.
    public func signedOut() {
        isSignedIn = false
        paths = [:]
        player = nil
    }

    /// Push a route onto the current tab's stack.
    public func navigate(to route: Route) {
        paths[selectedTab, default: NavigationPath()].append(route)
    }

    /// Present the player.
    public func play(_ request: PlaybackRequest) {
        player = PlayerPresentation(request: request)
    }

    /// Build the item and engine for a request.
    ///
    /// Prefers a completed download over the network for the same file, so
    /// playing something already on disk never touches the server -- which is
    /// the whole point of having downloaded it.
    public func playbackContext(
        for request: PlaybackRequest,
        container: String?
    ) -> (item: PlaybackItem, kind: PlaybackEngineKind)? {
        if let offline = downloads.offlineItem(for: request, container: container) {
            return (offline, EngineSelector.engine(for: offline))
        }
        guard let config = try? services.playback.playbackConfig(fileId: request.fileId),
            let item = PlaybackItem.from(
                config: config,
                container: container,
                request: request
            )
        else {
            return nil
        }
        return (item, EngineSelector.engine(for: item))
    }

    /// The engine implementation for a kind.
    ///
    /// Constructed here rather than inside the player so a test can hand
    /// `PlayerModel` a `FakePlaybackEngine` instead.
    public func makeEngine(kind: PlaybackEngineKind) -> any PlaybackEngine {
        switch kind {
        case .avPlayer: AVPlayerEngine()
        case .sampleBuffer: SampleBufferEngine()
        }
    }

    private static func isAuthenticated(_ state: SessionState) -> Bool {
        if case .authenticated = state { return true }
        return false
    }
}
