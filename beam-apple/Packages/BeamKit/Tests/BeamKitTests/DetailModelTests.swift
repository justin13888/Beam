import BeamCore
import BeamFFI
import BeamModel
import BeamTesting
import Foundation
import Testing

@testable import BeamDetail

/// One title's page, and the source choice it presents.
@MainActor
@Suite("Detail model")
struct DetailModelTests {
    private func makeModel(
        catalog: FakeCatalogRepository,
        playback: FakePlaybackRepository
    ) -> DetailModel {
        DetailModel(
            mediaId: "movie-1",
            catalog: catalog,
            playback: playback,
            quality: .best
        )
    }

    @Test("a title loads with its sources and the core's pick")
    func loadsSourcesAndSelection() async {
        let catalog = FakeCatalogRepository()
        catalog.setDetail(.movie(summary: Fixtures.movie()), for: "movie-1")
        let playback = FakePlaybackRepository()
        let playable = Fixtures.source(fileId: "file-1")
        playback.setSources([playable, Fixtures.source(fileId: "file-2")], for: "movie-1")
        playback.setSelection(Fixtures.selection(source: playable))

        let model = makeModel(catalog: catalog, playback: playback)
        await model.load()

        #expect(model.sources.count == 2)
        #expect(model.effectiveFileId == "file-1")
        #expect(model.hasPlayableSource)
    }

    @Test("a rejected source cannot be chosen")
    func rejectedSourceIsNotSelectable() async {
        // Offering the choice and then failing to play it would be worse than
        // not offering it -- and the reason is already on screen beside it.
        let catalog = FakeCatalogRepository()
        catalog.setDetail(.movie(summary: Fixtures.movie()), for: "movie-1")
        let playback = FakePlaybackRepository()
        let playable = Fixtures.source(fileId: "file-1")
        let unplayable = Fixtures.source(fileId: "file-2", videoCodec: "vc1")
        playback.setSources([playable, unplayable], for: "movie-1")
        playback.setSelection(
            Fixtures.selection(
                source: playable,
                rejected: [Fixtures.rejection(fileId: "file-2")]
            )
        )

        let model = makeModel(catalog: catalog, playback: playback)
        await model.load()
        model.choose(fileId: "file-2")

        #expect(model.effectiveFileId == "file-1", "a rejected source was chosen")
    }

    @Test("a playable source the viewer picks wins over the core's pick")
    func explicitChoiceWins() async {
        let catalog = FakeCatalogRepository()
        catalog.setDetail(.movie(summary: Fixtures.movie()), for: "movie-1")
        let playback = FakePlaybackRepository()
        let first = Fixtures.source(fileId: "file-1")
        let second = Fixtures.source(fileId: "file-2")
        playback.setSources([first, second], for: "movie-1")
        playback.setSelection(Fixtures.selection(source: first))

        let model = makeModel(catalog: catalog, playback: playback)
        await model.load()
        model.choose(fileId: "file-2")

        #expect(model.effectiveFileId == "file-2")
    }

    @Test("nothing playable is explained rather than left as a dead button")
    func unplayableTitleExplainsItself() async {
        let catalog = FakeCatalogRepository()
        catalog.setDetail(.movie(summary: Fixtures.movie()), for: "movie-1")
        let playback = FakePlaybackRepository()
        playback.setSources([Fixtures.source(fileId: "file-1", videoCodec: "vc1")], for: "movie-1")
        playback.setSelection(nil)

        let model = makeModel(catalog: catalog, playback: playback)
        await model.load()

        #expect(!model.hasPlayableSource)
        #expect(model.unplayableReason != nil)
        #expect(model.playbackRequest() == nil)
    }

    @Test("a show opens on its first season")
    func showOpensOnFirstSeason() async {
        let catalog = FakeCatalogRepository()
        let episode = EpisodeSummary(
            id: "episode-1",
            episodeNumber: 1,
            title: "Pole to Pole",
            description: nil,
            thumbnailUrl: nil,
            airDate: nil,
            durationSecs: 3000,
            fileId: "file-9"
        )
        catalog.setDetail(
            .show(
                summary: Fixtures.show(),
                seasons: [
                    SeasonSummary(
                        seasonNumber: 2,
                        posterUrl: nil,
                        episodeRuntimeMinutes: 50,
                        genres: [],
                        episodes: [episode]
                    )
                ]
            ),
            for: "movie-1"
        )
        let model = makeModel(catalog: catalog, playback: FakePlaybackRepository())

        await model.load()

        // The first season a show has, not the literal number one: a show whose
        // first indexed season is 2 should not open on an empty season 1.
        #expect(model.selectedSeason == 2)
        #expect(model.episodesInSelectedSeason.count == 1)
    }

    @Test("an episode with no indexed file yields no playback request")
    func unindexedEpisodeCannotPlay() async {
        let catalog = FakeCatalogRepository()
        let episode = EpisodeSummary(
            id: "episode-1",
            episodeNumber: 1,
            title: "Missing",
            description: nil,
            thumbnailUrl: nil,
            airDate: nil,
            durationSecs: nil,
            fileId: nil
        )
        catalog.setDetail(
            .show(
                summary: Fixtures.show(),
                seasons: [
                    SeasonSummary(
                        seasonNumber: 1,
                        posterUrl: nil,
                        episodeRuntimeMinutes: nil,
                        genres: [],
                        episodes: [episode]
                    )
                ]
            ),
            for: "movie-1"
        )
        let model = makeModel(catalog: catalog, playback: FakePlaybackRepository())
        await model.load()

        #expect(model.playbackRequest(for: episode) == nil)
    }
}
