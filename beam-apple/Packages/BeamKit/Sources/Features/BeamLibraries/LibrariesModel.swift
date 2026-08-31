import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Every library the server exposes.
@MainActor
@Observable
public final class LibrariesModel {
    /// The libraries.
    public private(set) var libraries: LoadState<[LibrarySummary]> = .idle

    @ObservationIgnored private let catalog: any CatalogRepository

    /// Build a model over the catalogue seam.
    public init(catalog: any CatalogRepository) {
        self.catalog = catalog
    }

    /// Load the library list.
    public func load() async {
        libraries = .loading
        do {
            libraries = .loaded(try await catalog.libraries())
        } catch {
            let failure = BeamFailure.from(error)
            libraries = .failed(failure.message)
        }
    }
}
