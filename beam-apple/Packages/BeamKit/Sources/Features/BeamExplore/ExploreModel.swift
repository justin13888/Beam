import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Search, filter and page through the whole catalogue.
///
/// The debounce and the page size are not arbitrary: 300ms and 40 match
/// `beam-web`'s explore page and `beam-android`'s, so the same typing produces
/// the same requests on every client and the server's rate limiting behaves
/// consistently. `GET /v1/media` is one of only two rate-limited route classes.
@MainActor
@Observable
public final class ExploreModel {
    /// How long to wait after a keystroke before searching.
    ///
    /// 300ms matches `beam-web`'s explore page and `beam-android`'s, so the
    /// same typing produces the same requests on every client.
    public static let defaultSearchDebounce: Duration = .milliseconds(300)

    /// The results so far, accumulated across pages.
    public private(set) var results: LoadState<[MediaSummary]> = .idle
    /// Every genre in the library, for the filter control.
    public private(set) var genres: [String] = []
    /// Whether another page exists.
    public private(set) var hasMore = false
    /// Whether a page request is in flight.
    public private(set) var isLoadingMore = false

    /// The current search text.
    public var searchText = "" {
        didSet {
            guard searchText != oldValue else { return }
            scheduleSearch()
        }
    }
    /// The selected genre, or `nil` for all.
    public var genre: String? { didSet { restart() } }
    /// The selected media type, or `nil` for all.
    public var mediaType: MediaTypeFilter? { didSet { restart() } }
    /// The sort field.
    public var sortBy: MediaSortField = .title { didSet { restart() } }
    /// The sort direction.
    public var sortOrder: BeamSortOrder = .ascending { didSet { restart() } }

    // `@ObservationIgnored` because none of these are view state, and because
    // `@Observable`'s generated accessors are main-actor isolated -- which
    // makes a stored task handle unreachable from `deinit`.
    @ObservationIgnored private let catalog: any CatalogRepository
    @ObservationIgnored private var cursor: String?
    @ObservationIgnored private let searchDebounce: Duration
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var pageTask: Task<Void, Never>?

    /// The debounce and fetch currently in flight, if any.
    ///
    /// Exposed so a test can await the work rather than sleep for longer than
    /// it thinks the work takes. A wall-clock sleep is flaky under load and
    /// hides the seam -- and this one did exactly that, passing locally and
    /// failing on a slower CI machine.
    @ObservationIgnored public private(set) var pendingWork: Task<Void, Never>?

    /// Build a model over the catalogue seam.
    ///
    /// - Parameter searchDebounce: injected so a test can make the wait
    ///   negligible, or long enough that "has not searched yet" is a fact
    ///   rather than a race.
    public init(
        catalog: any CatalogRepository,
        initialGenre: String? = nil,
        searchDebounce: Duration = ExploreModel.defaultSearchDebounce
    ) {
        self.catalog = catalog
        self.searchDebounce = searchDebounce
        self.genre = initialGenre
    }

    deinit {
        searchTask?.cancel()
        pageTask?.cancel()
    }

    /// Load the genre list and the first page.
    public func load() async {
        genres = (try? await catalog.genres()) ?? []
        await loadFirstPage()
    }

    /// Fetch the next page, if there is one.
    ///
    /// Guarded on `isLoadingMore` rather than debounced: a list can fire this
    /// several times while the user flicks, and without the guard each flick
    /// would issue a duplicate request for the same cursor.
    public func loadMore() {
        guard hasMore, !isLoadingMore, let cursor else { return }
        isLoadingMore = true
        pageTask?.cancel()
        let task = Task { [weak self] in
            guard let self else { return }
            await self.fetch(after: cursor, appending: true)
            self.isLoadingMore = false
        }
        pageTask = task
        pendingWork = task
    }

    private func scheduleSearch() {
        searchTask?.cancel()
        let task = Task { [weak self] in
            guard let self else { return }
            try? await Task.sleep(for: self.searchDebounce)
            guard !Task.isCancelled else { return }
            await self.loadFirstPage()
        }
        searchTask = task
        pendingWork = task
    }

    private func restart() {
        searchTask?.cancel()
        let task = Task { [weak self] in
            guard let self else { return }
            await self.loadFirstPage()
        }
        searchTask = task
        pendingWork = task
    }

    private func loadFirstPage() async {
        cursor = nil
        results = .loading
        await fetch(after: nil, appending: false)
    }

    private func fetch(after: String?, appending: Bool) async {
        do {
            let page = try await catalog.browse(
                query: .make(
                    after: after,
                    sortBy: sortBy,
                    sortOrder: sortOrder,
                    mediaType: mediaType,
                    genre: genre,
                    query: searchText.trimmingCharacters(in: .whitespaces)
                )
            )
            let existing = appending ? (results.value ?? []) : []
            results = .loaded(existing + page.items)
            cursor = page.endCursor
            hasMore = page.hasNextPage
        } catch {
            // A failed *next* page keeps what is already on screen: throwing
            // away forty results because the forty-first request failed would
            // lose the user's place for no reason.
            if appending {
                hasMore = false
            } else {
                results = .failed(BeamFailure.from(error).message)
            }
        }
    }
}
