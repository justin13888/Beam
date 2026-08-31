import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// The home screen's three rows.
///
/// The three loads run concurrently rather than in sequence: they are
/// independent requests and running them one after another would make the
/// screen three round trips slow instead of one. Each row keeps its own
/// `LoadState`, so one failing row does not blank the other two -- a server
/// that cannot answer "top rated" can still show what you were watching.
@MainActor
@Observable
public final class HomeModel {
    /// Titles part-way watched.
    public private(set) var continueWatching: LoadState<[ContinueWatchingEntry]> = .idle
    /// The most recently added titles.
    public private(set) var recentlyAdded: LoadState<[MediaSummary]> = .idle
    /// The highest rated titles.
    public private(set) var topRated: LoadState<[MediaSummary]> = .idle

    /// How many titles each row shows.
    public static let rowLimit: UInt32 = 20

    private let catalog: any CatalogRepository
    private let playback: any PlaybackRepository

    /// Build a model over the catalogue and playback seams.
    public init(catalog: any CatalogRepository, playback: any PlaybackRepository) {
        self.catalog = catalog
        self.playback = playback
    }

    /// Load every row.
    public func load() async {
        continueWatching = .loading
        recentlyAdded = .loading
        topRated = .loading

        async let watching = loadContinueWatching()
        async let recent = loadBrowse(sortBy: .dateAdded, order: .descending)
        async let rated = loadBrowse(sortBy: .rating, order: .descending)

        let (watchingResult, recentResult, ratedResult) = await (watching, recent, rated)
        continueWatching = watchingResult
        recentlyAdded = recentResult
        topRated = ratedResult
    }

    private func loadContinueWatching() async -> LoadState<[ContinueWatchingEntry]> {
        do {
            return .loaded(try await playback.continueWatching(limit: Self.rowLimit))
        } catch {
            return .failed(BeamFailure.from(error).message)
        }
    }

    private func loadBrowse(
        sortBy: MediaSortField,
        order: BeamSortOrder
    ) async -> LoadState<[MediaSummary]> {
        do {
            let page = try await catalog.browse(
                query: .make(first: Self.rowLimit, sortBy: sortBy, sortOrder: order)
            )
            return .loaded(page.items)
        } catch {
            return .failed(BeamFailure.from(error).message)
        }
    }
}
