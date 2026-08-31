import BeamModel
import BeamPlayback
import Foundation
import SwiftUI

/// A `PlaybackEngine` with no decoder behind it.
///
/// The reason the engine protocol exists. `PlayerModel` can be driven through
/// load, play, seek, track selection and failure with no VideoToolbox session,
/// no network and no file -- which is what makes the player screen testable at
/// all, and mirrors the discipline `PlayerTest.kt` applies by stubbing
/// `PlayerProvider.exoPlayer()` to a failure.
///
/// Scaffolding, never the subject of a test.
@MainActor
public final class FakePlaybackEngine: PlaybackEngine {
    public private(set) var snapshot = PlaybackSnapshot()
    public var onSnapshotChange: (@MainActor (PlaybackSnapshot) -> Void)?

    /// Every item this engine was asked to load, in order.
    public private(set) var loaded: [PlaybackItem] = []
    /// Every position it was asked to seek to, in order.
    public private(set) var seeks: [Double] = []
    /// Whether ``stop()`` has been called.
    public private(set) var didStop = false

    /// What ``load(_:)`` should throw, if anything.
    public var loadFailure: Error?
    /// The duration to report once loaded.
    public var duration: Double = 6240

    /// Tracks to report once loaded.
    public var audioTracks: [PlaybackTrack] = []
    /// Subtitle tracks to report once loaded.
    public var subtitleTracks: [PlaybackTrack] = []

    /// An engine with nothing loaded.
    public init() {}

    public func makeVideoView() -> AnyView {
        AnyView(Color.black)
    }

    public func load(_ item: PlaybackItem) async throws {
        if let loadFailure {
            update { $0.status = .failed(String(describing: loadFailure)) }
            throw loadFailure
        }
        loaded.append(item)
        update {
            $0.status = .paused
            $0.duration = duration
            $0.position = item.startPositionSeconds
            $0.audioTracks = audioTracks
            $0.subtitleTracks = subtitleTracks
            $0.selectedAudioTrackID = audioTracks.first(where: \.isDefault)?.id
        }
    }

    public func play() {
        update { $0.status = .playing }
    }

    public func pause() {
        update { $0.status = .paused }
    }

    public func seek(to seconds: Double) async {
        seeks.append(seconds)
        update { $0.position = max(0, seconds) }
    }

    public func selectAudioTrack(id: String) {
        update { $0.selectedAudioTrackID = id }
    }

    public func selectSubtitleTrack(id: String?) {
        update { $0.selectedSubtitleTrackID = id }
    }

    public func stop() {
        didStop = true
        update { $0.status = .idle }
    }

    /// Drive the engine to a state a test needs, such as reaching the end.
    public func simulate(_ mutate: (inout PlaybackSnapshot) -> Void) {
        update(mutate)
    }

    private func update(_ mutate: (inout PlaybackSnapshot) -> Void) {
        mutate(&snapshot)
        onSnapshotChange?(snapshot)
    }
}
