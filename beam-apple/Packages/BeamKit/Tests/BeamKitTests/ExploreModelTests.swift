import BeamCore
import BeamFFI
import BeamTesting
import Foundation
import Testing

@testable import BeamExplore

/// Search, filtering and paging.
@MainActor
@Suite("Explore model")
struct ExploreModelTests {
    private func catalog() -> FakeCatalogRepository {
        FakeCatalogRepository(items: [
            Fixtures.movie(id: "1", title: "The Third Man"),
            Fixtures.movie(id: "2", title: "Third Star"),
            Fixtures.show(id: "3", title: "Pole to Pole"),
        ])
    }

    @Test("typing does not search until the debounce elapses")
    func debounceDelaysTheSearch() async throws {
        // Every keystroke issuing a request would hammer one of only two
        // rate-limited route classes on the server.
        //
        // The debounce is injected as a full minute rather than waited out: it
        // makes "has not searched yet" a fact about the code instead of a race
        // the test wins on a fast machine and loses on a slow one.
        let catalog = catalog()
        let model = ExploreModel(catalog: catalog, searchDebounce: .seconds(60))
        await model.load()

        model.searchText = "T"
        model.searchText = "Th"
        model.searchText = "Thi"

        #expect(catalog.lastQuery()?.query == nil, "searched before the debounce elapsed")
    }

    @Test("only the last keystroke reaches the server")
    func debounceCoalesces() async throws {
        // Three keystrokes, one request, and it carries the final text --
        // which is the whole point of the debounce.
        let catalog = catalog()
        let model = ExploreModel(catalog: catalog, searchDebounce: .zero)

        model.searchText = "T"
        model.searchText = "Th"
        model.searchText = "Thi"
        await model.pendingWork?.value

        #expect(catalog.lastQuery()?.query == "Thi")
    }

    @Test("an empty search asks for no query rather than for an empty one")
    func emptySearchSendsNoQuery() async throws {
        // Sending "" would have the server match every title against an empty
        // string rather than skip the filter, which is a different and much
        // slower query.
        let catalog = catalog()
        let model = ExploreModel(catalog: catalog)

        await model.load()

        #expect(catalog.lastQuery()?.query == nil)
    }

    @Test("filters reach the query the server is actually asked")
    func filtersReachTheWire() async throws {
        let catalog = catalog()
        let model = ExploreModel(catalog: catalog)
        await model.load()

        model.genre = "Documentary"
        await model.pendingWork?.value

        let query = catalog.lastQuery()
        #expect(query?.genre == "Documentary")
        #expect(query?.first == BrowseQuery.pageSize)
    }

    @Test("a search narrows the results")
    func searchNarrowsResults() async throws {
        // Awaited rather than slept through. The sleep this replaced passed
        // locally and failed on a slower CI runner, which is exactly the
        // failure mode "never sleep for ordering" exists to prevent.
        let model = ExploreModel(catalog: catalog(), searchDebounce: .zero)
        await model.load()
        #expect(model.results.value?.count == 3)

        model.searchText = "Third"
        await model.pendingWork?.value

        #expect(model.results.value?.count == 2)
    }

    @Test("a failure is shown and can be retried")
    func failureIsSurfaced() async {
        let catalog = catalog()
        catalog.fail(with: BeamFailure(message: "server unreachable", isRetryable: true))
        let model = ExploreModel(catalog: catalog)

        await model.load()

        #expect(model.results.failure == "server unreachable")

        catalog.fail(with: nil)
        await model.load()
        #expect(model.results.value?.isEmpty == false)
    }

    @Test("an initial genre is applied to the first request")
    func initialGenre() async {
        // A library opens as the catalogue filtered, so the filter has to be in
        // the very first query -- loading everything and then narrowing would
        // fetch the whole library first.
        let catalog = catalog()
        let model = ExploreModel(catalog: catalog, initialGenre: "Documentary")

        await model.load()

        #expect(catalog.lastQuery()?.genre == "Documentary")
    }
}
