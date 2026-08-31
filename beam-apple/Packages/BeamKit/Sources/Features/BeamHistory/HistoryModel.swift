import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Everything this person has watched, newest first.
///
/// Offset paged rather than cursor paged, because that is what
/// `GET /v1/history` offers -- the catalogue's Relay cursors do not extend to
/// it. The page size matches `beam-web` and `beam-android`.
@MainActor
@Observable
public final class HistoryModel {
    /// How many entries a page holds.
    public static let pageSize: UInt32 = 50

    /// The entries loaded so far.
    public private(set) var entries: LoadState<[HistoryEntry]> = .idle
    /// How many entries the server holds in total.
    public private(set) var total: UInt64 = 0
    /// Whether a page request is in flight.
    public private(set) var isLoadingMore = false

    /// Whether there is another page to fetch.
    public var hasMore: Bool {
        UInt64(entries.value?.count ?? 0) < total
    }

    @ObservationIgnored private let playback: any PlaybackRepository

    /// Build a model over the playback seam.
    public init(playback: any PlaybackRepository) {
        self.playback = playback
    }

    /// Load the first page.
    public func load() async {
        entries = .loading
        do {
            let page = try await playback.history(limit: Self.pageSize, offset: 0)
            entries = .loaded(page.items)
            total = page.total
        } catch {
            entries = .failed(BeamFailure.from(error).message)
        }
    }

    /// Load the next page, if there is one.
    public func loadMore() async {
        guard hasMore, !isLoadingMore, let existing = entries.value else { return }
        isLoadingMore = true
        defer { isLoadingMore = false }
        do {
            let page = try await playback.history(
                limit: Self.pageSize,
                offset: UInt32(existing.count)
            )
            entries = .loaded(existing + page.items)
            total = page.total
        } catch {
            // Keep what is on screen; a failed next page is not a reason to
            // discard the pages that already arrived.
            total = UInt64(existing.count)
        }
    }
}
