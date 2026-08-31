import BeamFFI
import Foundation

/// The core's sort direction.
///
/// Aliased because Foundation exports a `SortOrder` of its own, and an
/// unqualified mention in a view resolves to neither.
public typealias BeamSortOrder = BeamFFI.SortOrder

extension BrowseQuery {
    /// How many titles a page holds.
    ///
    /// Matches `beam-web` and `beam-android`, so scrolling to the same place in
    /// a library issues the same requests on every client and the server's
    /// cursors behave identically.
    public static let pageSize: UInt32 = 40

    /// A query with everything that was not asked for left unset.
    ///
    /// `BrowseQuery` has eleven optional fields, and a struct literal spelling
    /// all of them appears in five screens. This is the only place the
    /// defaults are written, so adding a twelfth filter does not mean editing
    /// five call sites that never used it.
    public static func make(
        first: UInt32 = BrowseQuery.pageSize,
        after: String? = nil,
        sortBy: MediaSortField = .title,
        sortOrder: BeamSortOrder = .ascending,
        mediaType: MediaTypeFilter? = nil,
        genre: String? = nil,
        year: UInt32? = nil,
        yearFrom: UInt32? = nil,
        yearTo: UInt32? = nil,
        query: String? = nil,
        minRating: UInt32? = nil
    ) -> BrowseQuery {
        BrowseQuery(
            first: first,
            after: after,
            sortBy: sortBy,
            sortOrder: sortOrder,
            mediaType: mediaType,
            genre: genre,
            year: year,
            yearFrom: yearFrom,
            yearTo: yearTo,
            // An empty search is no search: sending "" would have the server
            // match every title against an empty string rather than skipping
            // the filter, which is a different and much slower query.
            query: (query?.isEmpty ?? true) ? nil : query,
            minRating: minRating
        )
    }
}
