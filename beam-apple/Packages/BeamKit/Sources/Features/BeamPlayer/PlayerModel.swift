import BeamCore
import BeamFFI
import BeamModel
import BeamPlayback
import Foundation
import SwiftUI

/// Drives one playback session.
///
/// It never constructs an engine itself: one is handed in. That is what makes
/// this testable with `FakePlaybackEngine` and no decoder, and it is the same
/// discipline `PlayerTest.kt` applies by stubbing `PlayerProvider.exoPlayer()`
/// to a failure -- a player model that reached for a real decoder just to hold
/// state could only be tested on a device.
@MainActor
@Observable
public final class PlayerModel {
    /// The engine's current state.
    public private(set) var snapshot = PlaybackSnapshot()
    /// Which engine is driving, for the diagnostics row.
    public private(set) var engineKind: PlaybackEngineKind = .avPlayer
    /// What is playing.
    public private(set) var request: PlaybackRequest
    /// The next episode, once one has been resolved.
    public private(set) var upNext: EpisodeSummary?
    /// Whether the controls are on screen.
    public var areControlsVisible = true

    @ObservationIgnored private let engine: any PlaybackEngine
    @ObservationIgnored private let playback: any PlaybackRepository
    @ObservationIgnored private let catalog: any CatalogRepository
    @ObservationIgnored private let reporter: ProgressReporter
    @ObservationIgnored private let nowPlaying: NowPlayingCenter
    @ObservationIgnored private let autoplayNextEpisode: Bool

    /// Build a model for one request.
    public init(
        request: PlaybackRequest,
        engine: any PlaybackEngine,
        engineKind: PlaybackEngineKind,
        playback: any PlaybackRepository,
        catalog: any CatalogRepository,
        nowPlaying: NowPlayingCenter = NowPlayingCenter(),
        autoplayNextEpisode: Bool = true
    ) {
        self.request = request
        self.engine = engine
        self.engineKind = engineKind
        self.playback = playback
        self.catalog = catalog
        self.reporter = ProgressReporter(playback: playback)
        self.nowPlaying = nowPlaying
        self.autoplayNextEpisode = autoplayNextEpisode
    }

    /// The video surface for the engine in use.
    public func videoView() -> AnyViewBox {
        AnyViewBox(engine.makeVideoView())
    }

    /// Open the item and start playing.
    public func start(item: PlaybackItem) async {
        engine.onSnapshotChange = { [weak self] snapshot in
            self?.apply(snapshot)
        }

        nowPlaying.activate(
            commands: NowPlayingCenter.Commands(
                play: { [weak self] in self?.play() },
                pause: { [weak self] in self?.pause() },
                seek: { [weak self] seconds in self?.seek(to: seconds) },
                skipForward: { [weak self] in self?.skip(by: NowPlayingCenter.skipInterval) },
                skipBackward: { [weak self] in self?.skip(by: -NowPlayingCenter.skipInterval) }
            )
        )

        do {
            try await engine.load(item)
            engine.play()
            reporter.start(fileId: request.fileId) { [weak self] in
                (self?.snapshot.position ?? 0, self?.snapshot.duration)
            }
            await resolveUpNext()
        } catch {
            let failure = BeamFailure.from(error)
            let message =
                (error as? PlaybackEngineError)?.message ?? failure.message
            var failed = snapshot
            failed.status = .failed(message)
            apply(failed)
        }
    }

    /// Resume.
    public func play() {
        engine.play()
    }

    /// Pause, and report the position immediately.
    ///
    /// Forced rather than left to the 15-second sampler: a pause is exactly
    /// where someone stops, and losing up to fifteen seconds of it is the
    /// difference between resuming where they were and resuming before it.
    public func pause() {
        engine.pause()
        Task { await flushProgress() }
    }

    /// Toggle between playing and paused.
    public func togglePlayPause() {
        if case .playing = snapshot.status { pause() } else { play() }
    }

    /// Move to an absolute position.
    public func seek(to seconds: Double) {
        Task {
            await engine.seek(to: seconds)
            await flushProgress()
        }
    }

    /// Move by a relative amount, clamped to the item.
    public func skip(by seconds: Double) {
        let target = max(0, min(snapshot.position + seconds, snapshot.duration ?? .infinity))
        seek(to: target)
    }

    /// Choose an audio track.
    public func selectAudioTrack(id: String) {
        engine.selectAudioTrack(id: id)
    }

    /// Choose a subtitle track, or turn them off.
    public func selectSubtitleTrack(id: String?) {
        engine.selectSubtitleTrack(id: id)
    }

    /// Tear down, reporting a final position.
    public func stop() async {
        await flushProgress()
        reporter.stop()
        nowPlaying.deactivate()
        engine.stop()
    }

    /// Whether the next episode should be offered.
    ///
    /// The last thirty seconds, which is where an end card belongs: earlier
    /// interrupts the film, later is after the viewer has already reached for
    /// the back button.
    public var shouldOfferUpNext: Bool {
        guard upNext != nil, let duration = snapshot.duration, duration > 0 else { return false }
        return snapshot.position >= duration - 30 || snapshot.status == .ended
    }

    private func apply(_ snapshot: PlaybackSnapshot) {
        let wasEnded = self.snapshot.status == .ended
        self.snapshot = snapshot
        nowPlaying.update(request: request, snapshot: snapshot)

        if !wasEnded, snapshot.status == .ended {
            Task { await flushProgress() }
        }
    }

    private func flushProgress() async {
        await reporter.flush(
            positionSeconds: snapshot.position,
            durationSeconds: snapshot.duration
        )
    }

    private func resolveUpNext() async {
        guard autoplayNextEpisode,
            let mediaId = request.mediaId,
            let episodeId = request.episodeId
        else {
            return
        }
        upNext = try? await catalog.upNext(showId: mediaId, currentEpisodeId: episodeId)
    }
}

/// Carries an erased view out of the model.
///
/// `AnyView` in an `@Observable` property would make every snapshot change
/// re-evaluate the video surface; boxing it means the surface is created once
/// and the observation tracks the box's identity rather than the view.
public struct AnyViewBox: Identifiable {
    /// Stable for the life of the box.
    public let id = UUID()
    /// The erased surface.
    public let view: AnyView

    /// Wrap a view.
    public init(_ view: AnyView) {
        self.view = view
    }
}
