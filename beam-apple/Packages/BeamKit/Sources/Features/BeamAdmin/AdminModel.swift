import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// The operator screen: status, libraries, users and the log.
///
/// The screen is not hidden from non-administrators. The server guards these
/// routes with a 403, and that is the guard that matters -- a client-side check
/// would be advisory at best, and hiding the screen would mean an administrator
/// whose role changed had no way to see why their access stopped working.
@MainActor
@Observable
public final class AdminModel {
    /// The dashboard snapshot.
    public private(set) var status: LoadState<AdminStatus> = .idle
    /// Every library, so one can be scanned or removed.
    public private(set) var libraries: [LibrarySummary] = []
    /// User accounts.
    public private(set) var users: [AdminUser] = []
    /// Recent log lines.
    public private(set) var logs: [AdminLogEntry] = []
    /// Whether the signed-in account may do any of this.
    public private(set) var isForbidden = false
    /// Set when an action failed, for a transient banner.
    public var actionMessage: String?

    /// A new library's name, as typed.
    public var newLibraryName = ""
    /// A new library's root path on the server, as typed.
    public var newLibraryPath = ""

    @ObservationIgnored private let admin: any AdminRepository
    @ObservationIgnored private let catalog: any CatalogRepository

    /// How many log lines the screen shows.
    public static let logLimit: UInt32 = 100

    /// Build a model over the admin seams.
    public init(admin: any AdminRepository, catalog: any CatalogRepository) {
        self.admin = admin
        self.catalog = catalog
    }

    /// Load everything the screen shows.
    public func load() async {
        status = .loading
        isForbidden = false
        do {
            status = .loaded(try await admin.status())
        } catch {
            let failure = BeamFailure.from(error)
            isForbidden = failure.isForbidden
            status = .failed(failure.message)
            // A 403 means nothing else here will work either; asking anyway
            // would produce four more identical failures.
            if failure.isForbidden { return }
        }

        libraries = (try? await catalog.libraries()) ?? []
        users = (try? await admin.users(limit: 100, offset: 0))?.items ?? []
        logs = (try? await admin.logs(limit: Self.logLimit, offset: 0)) ?? []
    }

    /// Scan one library, and report how many files it added.
    public func scan(libraryId: String) async {
        do {
            let added = try await admin.scanLibrary(id: libraryId)
            actionMessage =
                added == 0 ? "Scan finished; nothing new." : "Scan added \(added) files."
            libraries = (try? await catalog.libraries()) ?? libraries
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// Add a library rooted at a path on the server.
    public func createLibrary() async {
        let name = newLibraryName.trimmingCharacters(in: .whitespaces)
        let path = newLibraryPath.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, !path.isEmpty else { return }
        do {
            _ = try await admin.createLibrary(name: name, rootPath: path)
            newLibraryName = ""
            newLibraryPath = ""
            libraries = (try? await catalog.libraries()) ?? libraries
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// Remove a library.
    public func deleteLibrary(id libraryId: String) async {
        do {
            try await admin.deleteLibrary(id: libraryId)
            libraries = (try? await catalog.libraries()) ?? libraries
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// Disable or re-enable an account.
    public func setDisabled(_ disabled: Bool, userId: String) async {
        do {
            try await admin.setUserDisabled(userId: userId, disabled: disabled)
            users = (try? await admin.users(limit: 100, offset: 0))?.items ?? users
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }
}
