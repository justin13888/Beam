import BeamCore
import BeamFFI
import Foundation

/// Samples playback position and hands it to the core.
///
/// Deliberately thin. The throttle, the coalescing and the durable retry queue
/// all live in `beam-client-core`'s `progress` module, so an interrupted
/// report survives a crash and behaves identically on Android. This only
/// decides *when to sample*, and forces a report at the moments where losing
/// one would be visible: a pause, a seek, the end of an item, and going away.
///
/// The 15-second interval matches `beam-web`'s `usePlaybackBeacon` and
/// `beam-android`'s `ProgressReporter`, so "continue watching" behaves the
/// same wherever the person was last watching.
@MainActor
public final class ProgressReporter {
    /// How often to sample while playing.
    public static let interval: Duration = .seconds(15)

    private let playback: any PlaybackRepository
    private var ticker: Task<Void, Never>?
    private var fileId: String?

    /// Report through `playback`.
    public init(playback: any PlaybackRepository) {
        self.playback = playback
    }

    deinit {
        ticker?.cancel()
    }

    /// Begin sampling for `fileId`.
    ///
    /// - Parameter position: a closure read on each tick, rather than a value
    ///   pushed in, so the reporter always sees the engine's current position
    ///   and can never send a stale one.
    public func start(fileId: String, position: @escaping @MainActor () -> (Double, Double?)) {
        stop()
        self.fileId = fileId
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.interval)
                guard !Task.isCancelled, let self else { return }
                let (seconds, duration) = position()
                await self.report(positionSeconds: seconds, durationSeconds: duration, force: false)
            }
        }
    }

    /// Send a report now, bypassing the throttle.
    ///
    /// Used for pause, seek, end of item and backgrounding -- the moments a
    /// dropped report is the difference between resuming where someone was and
    /// resuming up to fifteen seconds earlier.
    public func flush(positionSeconds: Double, durationSeconds: Double?) async {
        await report(
            positionSeconds: positionSeconds, durationSeconds: durationSeconds, force: true)
    }

    /// Stop sampling.
    public func stop() {
        ticker?.cancel()
        ticker = nil
    }

    private func report(positionSeconds: Double, durationSeconds: Double?, force: Bool) async {
        guard let fileId, positionSeconds.isFinite, positionSeconds >= 0 else { return }
        // A failed report is not shown to the viewer and not retried here: the
        // core has already queued it durably, and an error banner over a film
        // because a progress beacon missed would be worse than the miss.
        _ = try? await playback.reportProgress(
            fileId: fileId,
            positionSeconds: positionSeconds,
            durationSeconds: durationSeconds,
            force: force
        )
    }
}
