import BeamFFI
import BeamModel
import BeamPlayback
import BeamTesting
import Foundation
import Testing

@testable import BeamPlayer

/// The player screen's behaviour, with no decoder behind it.
@MainActor
@Suite("Player model")
struct PlayerModelTests {
    private func makeModel(
        engine: FakePlaybackEngine,
        playback: FakePlaybackRepository = FakePlaybackRepository(),
        catalog: FakeCatalogRepository = FakeCatalogRepository(),
        request: PlaybackRequest = PlaybackRequest(
            fileId: "file-1",
            mediaId: "movie-1",
            title: "The Third Man",
            startPositionSeconds: 0
        ),
        autoplay: Bool = true
    ) -> PlayerModel {
        PlayerModel(
            request: request,
            engine: engine,
            engineKind: .avPlayer,
            playback: playback,
            catalog: catalog,
            autoplayNextEpisode: autoplay
        )
    }

    private var item: PlaybackItem {
        PlaybackItem(
            url: URL(string: "https://beam.invalid/v1/files/file-1/stream")!,
            request: PlaybackRequest(fileId: "file-1", title: "The Third Man")
        )
    }

    @Test("starting loads the item and begins playing")
    func startPlays() async {
        let engine = FakePlaybackEngine()
        let model = makeModel(engine: engine)

        await model.start(item: item)

        #expect(engine.loaded.count == 1)
        #expect(model.snapshot.status == .playing)
    }

    @Test("a resume position is handed to the engine, not applied afterwards")
    func resumePosition() async {
        // Seeking after load would show the opening frames before jumping,
        // which is what a viewer resuming an hour in should never see.
        let engine = FakePlaybackEngine()
        let request = PlaybackRequest(
            fileId: "file-1",
            title: "The Third Man",
            startPositionSeconds: 1200
        )
        let model = makeModel(engine: engine, request: request)

        await model.start(
            item: PlaybackItem(
                url: URL(string: "https://beam.invalid/v1/files/file-1/stream")!,
                startPositionSeconds: 1200,
                request: request
            )
        )

        #expect(engine.loaded.first?.startPositionSeconds == 1200)
        #expect(model.snapshot.position == 1200)
    }

    @Test("pausing reports the position immediately rather than waiting for the sampler")
    func pauseForcesAReport() async {
        // The 15-second sampler would lose up to fifteen seconds of a pause,
        // which is the difference between resuming where someone stopped and
        // resuming before it.
        let engine = FakePlaybackEngine()
        let playback = FakePlaybackRepository()
        let model = makeModel(engine: engine, playback: playback)
        await model.start(item: item)
        engine.simulate { $0.position = 640 }

        model.pause()
        await model.pendingWork?.value

        let forced = playback.reports().filter(\.forced)
        #expect(forced.contains { $0.position == 640 })
    }

    @Test("a load failure is shown rather than leaving a blank player")
    func loadFailureIsSurfaced() async {
        let engine = FakePlaybackEngine()
        engine.loadFailure = PlaybackEngineError.unsupportedVideo(
            detail: "This device cannot play V_MS/VFW/FOURCC in this container."
        )
        let model = makeModel(engine: engine)

        await model.start(item: item)

        guard case .failed(let message) = model.snapshot.status else {
            Issue.record("expected a failure, got \(model.snapshot.status)")
            return
        }
        #expect(message.contains("V_MS/VFW/FOURCC"))
    }

    @Test("skipping never runs past either end of the item")
    func skipIsClamped() async {
        let engine = FakePlaybackEngine()
        engine.duration = 100
        let model = makeModel(engine: engine)
        await model.start(item: item)

        model.skip(by: -30)
        await model.pendingWork?.value
        #expect(model.snapshot.position == 0)

        engine.simulate { $0.position = 95 }
        model.skip(by: 30)
        await model.pendingWork?.value
        #expect(model.snapshot.position == 100)
    }

    @Test("up next is offered near the end, and not before")
    func upNextTiming() async {
        let engine = FakePlaybackEngine()
        engine.duration = 600
        let catalog = FakeCatalogRepository()
        catalog.setUpNext(
            EpisodeSummary(
                id: "episode-2",
                episodeNumber: 2,
                title: "Pole to Pole",
                description: nil,
                thumbnailUrl: nil,
                airDate: nil,
                durationSecs: 600,
                fileId: "file-2"
            )
        )
        let model = makeModel(
            engine: engine,
            catalog: catalog,
            request: PlaybackRequest(
                fileId: "file-1",
                mediaId: "show-1",
                episodeId: "episode-1",
                title: "Pole to Pole"
            )
        )
        await model.start(item: item)

        engine.simulate { $0.position = 300 }
        #expect(!model.shouldOfferUpNext, "offered halfway through")

        engine.simulate { $0.position = 590 }
        #expect(model.shouldOfferUpNext)
    }

    @Test("up next is not resolved when autoplay is off")
    func autoplayRespected() async {
        let engine = FakePlaybackEngine()
        let catalog = FakeCatalogRepository()
        catalog.setUpNext(
            EpisodeSummary(
                id: "episode-2",
                episodeNumber: 2,
                title: "Next",
                description: nil,
                thumbnailUrl: nil,
                airDate: nil,
                durationSecs: nil,
                fileId: "file-2"
            )
        )
        let model = makeModel(
            engine: engine,
            catalog: catalog,
            request: PlaybackRequest(
                fileId: "file-1",
                mediaId: "show-1",
                episodeId: "episode-1",
                title: "Pole to Pole"
            ),
            autoplay: false
        )

        await model.start(item: item)

        #expect(model.upNext == nil)
    }

    @Test("stopping reports a final position and tears the engine down")
    func stopFlushesAndTearsDown() async {
        let engine = FakePlaybackEngine()
        let playback = FakePlaybackRepository()
        let model = makeModel(engine: engine, playback: playback)
        await model.start(item: item)
        engine.simulate { $0.position = 42 }

        await model.stop()

        #expect(engine.didStop)
        #expect(playback.reports().contains { $0.position == 42 && $0.forced })
    }
}
