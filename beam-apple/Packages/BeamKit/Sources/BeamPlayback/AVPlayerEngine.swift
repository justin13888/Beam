import AVFoundation
import AVKit
import BeamFFI
import BeamModel
import Foundation
import SwiftUI

/// The preferred engine: `AVPlayer`, wherever AVFoundation can open the file.
///
/// Using Apple's player where it is capable is a deliberate best practice, not
/// laziness. It brings Picture in Picture, AirPlay, the system transport and
/// remote-control integration, the Now Playing card, and buffering, seeking
/// and audio-session behaviour that has been tuned for a decade. Every one of
/// those would have to be rebuilt on top of `SampleBufferEngine`, and none of
/// them would be as good.
///
/// Two things here are Beam-specific and are the reason this is not simply
/// `VideoPlayer(player:)`:
///
/// - **The credential.** `beam-server` authenticates the stream endpoint with
///   the `beam_session` cookie and refuses tokens in the URL (FR-504).
///   `AVURLAssetHTTPCookiesKey` is how a cookie reaches AVFoundation's own
///   loader, which does its own range requests and never goes through
///   `URLSession`.
/// - **The trust decision.** AVFoundation exposes no `URLSession` delegate, so
///   a user-accepted certificate would be rejected before a byte was fetched.
///   `AVAssetResourceLoaderDelegate`'s authentication-challenge callback is the
///   one hook it offers, and it is what makes a self-hosted LAN server work.
@MainActor
public final class AVPlayerEngine: NSObject, PlaybackEngine {
    public private(set) var snapshot = PlaybackSnapshot()
    public var onSnapshotChange: (@MainActor (PlaybackSnapshot) -> Void)?

    private let player = AVPlayer()
    private var item: AVPlayerItem?
    private var timeObserver: Any?
    private var statusObservation: NSKeyValueObservation?
    private var bufferObservation: NSKeyValueObservation?
    private var endWatcher: Task<Void, Never>?
    private var resourceLoaderDelegate: TrustingResourceLoaderDelegate?
    // Cached when the item becomes ready. The synchronous accessor is
    // deprecated, and the async one cannot be called from `selectAudioTrack`,
    // which the transport menu invokes without awaiting.
    private var audibleGroup: AVMediaSelectionGroup?
    private var legibleGroup: AVMediaSelectionGroup?

    /// A player with no item loaded.
    public override init() {
        super.init()
        player.automaticallyWaitsToMinimizeStalling = true
    }

    deinit {
        // `deinit` is nonisolated and cannot touch main-actor state, but a
        // `Task` is `Sendable` and cancelling one is safe from anywhere. That
        // is why end-of-item is watched through `NotificationCenter`'s async
        // sequence rather than a block observer: a block observer would be
        // retained by the notification centre with no way to remove it here.
        endWatcher?.cancel()
    }

    public func makeVideoView() -> AnyView {
        AnyView(SystemPlayerView(player: player))
    }

    public func load(_ item: PlaybackItem) async throws {
        stop()
        update { $0.status = .loading }

        let asset = AVURLAsset(url: item.url, options: assetOptions(for: item))

        // Only for a pinned host: installing the delegate unconditionally
        // would put every request through our own evaluation, and the point of
        // the trust model is that the platform decides first.
        if !item.trustedFingerprints.isEmpty {
            let delegate = TrustingResourceLoaderDelegate(
                evaluator: CertificateTrustEvaluator(
                    trustedFingerprints: item.trustedFingerprints,
                    pinnedHost: item.pinnedHost
                )
            )
            asset.resourceLoader.setDelegate(delegate, queue: .main)
            resourceLoaderDelegate = delegate
        }

        let playerItem = AVPlayerItem(asset: asset)
        self.item = playerItem
        player.replaceCurrentItem(with: playerItem)
        observe(playerItem)

        if item.startPositionSeconds > 0 {
            await seek(to: item.startPositionSeconds)
        }
    }

    public func play() {
        player.play()
        update { $0.status = .playing }
    }

    public func pause() {
        player.pause()
        update { $0.status = .paused }
    }

    public func seek(to seconds: Double) async {
        let time = CMTime(seconds: max(0, seconds), preferredTimescale: 600)
        // Zero tolerance: a viewer who drags to a chapter boundary means that
        // boundary, and AVPlayer's default tolerance can land seconds away.
        await player.seek(to: time, toleranceBefore: .zero, toleranceAfter: .zero)
        update { $0.position = seconds }
    }

    public func selectAudioTrack(id: String) {
        select(id: id, in: .audible) { $0.selectedAudioTrackID = id }
    }

    public func selectSubtitleTrack(id: String?) {
        guard let id else {
            if let group = mediaGroup(.legible) {
                item?.select(nil, in: group)
            }
            update { $0.selectedSubtitleTrackID = nil }
            return
        }
        select(id: id, in: .legible) { $0.selectedSubtitleTrackID = id }
    }

    public func stop() {
        if let timeObserver {
            player.removeTimeObserver(timeObserver)
            self.timeObserver = nil
        }
        endWatcher?.cancel()
        endWatcher = nil
        statusObservation = nil
        bufferObservation = nil
        resourceLoaderDelegate = nil
        audibleGroup = nil
        legibleGroup = nil
        player.replaceCurrentItem(with: nil)
        item = nil
    }

    // MARK: - Internals

    private func assetOptions(for item: PlaybackItem) -> [String: Any] {
        var options: [String: Any] = [:]

        // The session cookie has to reach AVFoundation's own loader, which
        // never goes through URLSession and so never sees HTTPCookieStorage
        // unless told to. Any non-cookie header is passed through the private
        // header option, which is the only way AVFoundation accepts one.
        var cookies: [HTTPCookie] = []
        var otherHeaders: [String: String] = [:]
        for (name, value) in item.headers {
            if name.lowercased() == "cookie" {
                cookies.append(contentsOf: Self.cookies(from: value, url: item.url))
            } else {
                otherHeaders[name] = value
            }
        }
        if !cookies.isEmpty {
            options[AVURLAssetHTTPCookiesKey] = cookies
        }
        if !otherHeaders.isEmpty {
            options["AVURLAssetHTTPHeaderFieldsKey"] = otherHeaders
        }
        return options
    }

    /// Parse a `Cookie:` header value into cookies scoped to `url`.
    static func cookies(from headerValue: String, url: URL) -> [HTTPCookie] {
        guard let host = url.host() else { return [] }
        return headerValue.split(separator: ";").compactMap { pair in
            let trimmed = pair.trimmingCharacters(in: .whitespaces)
            guard let separator = trimmed.firstIndex(of: "=") else { return nil }
            let name = String(trimmed[trimmed.startIndex..<separator])
            let value = String(trimmed[trimmed.index(after: separator)...])
            guard !name.isEmpty, !value.isEmpty else { return nil }
            return HTTPCookie(properties: [
                .name: name,
                .value: value,
                .domain: host,
                .path: "/",
                // Not `.secure`: a self-hosted server on a LAN is routinely
                // plain HTTP, and marking the cookie secure would silently
                // drop it there.
                .version: "0",
            ])
        }
    }

    private func observe(_ playerItem: AVPlayerItem) {
        statusObservation = playerItem.observe(\.status, options: [.new]) { [weak self] item, _ in
            Task { @MainActor [weak self] in
                guard let self else { return }
                switch item.status {
                case .readyToPlay:
                    self.refreshTracks(from: item)
                    self.update {
                        $0.duration = item.duration.seconds.isFinite ? item.duration.seconds : nil
                        $0.isSeekable = !item.seekableTimeRanges.isEmpty
                        if case .loading = $0.status { $0.status = .paused }
                    }
                case .failed:
                    let message =
                        item.error?.localizedDescription
                        ?? "This file could not be played on this device."
                    self.update { $0.status = .failed(message) }
                default:
                    break
                }
            }
        }

        bufferObservation = playerItem.observe(\.loadedTimeRanges, options: [.new]) {
            [weak self] item, _ in
            Task { @MainActor [weak self] in
                guard let self else { return }
                let ahead =
                    item.loadedTimeRanges
                    .map { $0.timeRangeValue }
                    .map { CMTimeGetSeconds($0.start + $0.duration) }
                    .max() ?? 0
                self.update { $0.bufferedAhead = max(0, ahead - $0.position) }
            }
        }

        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.5, preferredTimescale: 600),
            queue: .main
        ) { [weak self] time in
            Task { @MainActor [weak self] in
                self?.update { $0.position = time.seconds }
            }
        }

        endWatcher = Task { [weak self] in
            let notifications = NotificationCenter.default.notifications(
                named: .AVPlayerItemDidPlayToEndTime,
                object: playerItem
            )
            for await _ in notifications {
                guard !Task.isCancelled else { return }
                await MainActor.run { self?.update { $0.status = .ended } }
            }
        }
    }

    private func refreshTracks(from item: AVPlayerItem) {
        Task { @MainActor in
            let audible = try? await item.asset.loadMediaSelectionGroup(for: .audible)
            let legible = try? await item.asset.loadMediaSelectionGroup(for: .legible)
            audibleGroup = audible
            legibleGroup = legible
            update {
                $0.audioTracks = Self.tracks(in: audible)
                $0.subtitleTracks = Self.tracks(in: legible)
            }
        }
    }

    private static func tracks(in group: AVMediaSelectionGroup?) -> [PlaybackTrack] {
        guard let group else { return [] }
        return group.options.enumerated().map { index, option in
            PlaybackTrack(
                // `displayName` is not unique -- two "English" tracks are
                // common -- so the index is part of the identity.
                id: "\(index):\(option.displayName)",
                label: option.displayName,
                languageCode: option.extendedLanguageTag,
                isDefault: index == 0,
                isPlayable: true
            )
        }
    }

    private func mediaGroup(_ characteristic: AVMediaCharacteristic) -> AVMediaSelectionGroup? {
        characteristic == .audible ? audibleGroup : legibleGroup
    }

    private func select(
        id: String,
        in characteristic: AVMediaCharacteristic,
        then apply: @escaping (inout PlaybackSnapshot) -> Void
    ) {
        guard let group = mediaGroup(characteristic),
            let index = Int(id.split(separator: ":").first.map(String.init) ?? ""),
            group.options.indices.contains(index)
        else {
            return
        }
        item?.select(group.options[index], in: group)
        update(apply)
    }

    private func update(_ mutate: (inout PlaybackSnapshot) -> Void) {
        mutate(&snapshot)
        onSnapshotChange?(snapshot)
    }
}

/// Applies Beam's trust model to AVFoundation's own loader.
///
/// `shouldWaitForResponseTo` is the only authentication hook `AVAssetResourceLoader`
/// offers, and without it a user-accepted certificate is rejected before the
/// first byte -- which is every self-hosted server on a LAN.
private final class TrustingResourceLoaderDelegate: NSObject, AVAssetResourceLoaderDelegate {
    private let evaluator: CertificateTrustEvaluator

    init(evaluator: CertificateTrustEvaluator) {
        self.evaluator = evaluator
    }

    func resourceLoader(
        _ resourceLoader: AVAssetResourceLoader,
        shouldWaitForResponseTo authenticationChallenge: URLAuthenticationChallenge
    ) -> Bool {
        guard let credential = evaluator.evaluate(authenticationChallenge) else {
            authenticationChallenge.sender?.continueWithoutCredential(for: authenticationChallenge)
            return true
        }
        authenticationChallenge.sender?.use(credential, for: authenticationChallenge)
        return true
    }
}

/// The system player surface, per platform.
///
/// `AVPlayerViewController` and `AVPlayerView` rather than SwiftUI's
/// `VideoPlayer`, because Picture in Picture, AirPlay routing and the full
/// transport bar come from the platform controllers and not from the SwiftUI
/// wrapper.
private struct SystemPlayerView: View {
    let player: AVPlayer

    var body: some View {
        #if os(iOS)
        IOSPlayerView(player: player).ignoresSafeArea()
        #else
        MacPlayerView(player: player)
        #endif
    }
}

#if os(iOS)
private struct IOSPlayerView: UIViewControllerRepresentable {
    let player: AVPlayer

    func makeUIViewController(context: Context) -> AVPlayerViewController {
        let controller = AVPlayerViewController()
        controller.player = player
        controller.allowsPictureInPicturePlayback = true
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        controller.videoGravity = .resizeAspect
        return controller
    }

    func updateUIViewController(_ controller: AVPlayerViewController, context: Context) {
        if controller.player !== player {
            controller.player = player
        }
    }
}
#else
private struct MacPlayerView: NSViewRepresentable {
    let player: AVPlayer

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.player = player
        view.controlsStyle = .floating
        view.allowsPictureInPicturePlayback = true
        view.videoGravity = .resizeAspect
        return view
    }

    func updateNSView(_ view: AVPlayerView, context: Context) {
        if view.player !== player {
            view.player = player
        }
    }
}
#endif
