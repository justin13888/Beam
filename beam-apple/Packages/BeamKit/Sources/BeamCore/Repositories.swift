import BeamFFI
import BeamModel
import Foundation

// The six seams every screen talks to, mirroring `Repositories.kt`.
//
// Screens depend on these protocols and never on `BeamClient`, so a view model
// can be driven by a stateful fake from `BeamTesting` with no server, no
// network and no session. Every method throws `BeamFailure` rather than the
// generated `BeamError`, so the "what do I show, can I retry, must they sign
// in" decision is made once here instead of in ten view models.

/// The registry of servers this device knows, and the session on each.
public protocol ServerRepository: Sendable {
    /// Load the registry from storage. Called once at launch.
    func restore() async throws -> [ServerSummary]
    /// Every known server.
    func listServers() throws -> [ServerSummary]
    /// Add a server, or return the existing entry for the same origin.
    func addServer(baseURL: String, displayName: String?) async throws -> ServerSummary
    /// Make `serverId` the one subsequent calls act on.
    func selectServer(id serverId: String) async throws
    /// Forget a server, its session and its trusted certificates.
    func removeServer(id serverId: String) async throws
    /// The URL to open in a web view to begin signing in.
    func loginURL(serverId: String) throws -> String
    /// Hand the cookie lifted from the web view to the core, which verifies it.
    func completeLogin(serverId: String, sessionCookie: String) async throws -> UserSummary
    /// End the session on one server.
    func logout(serverId: String) async throws
    /// The session state for one server.
    func sessionState(serverId: String) throws -> SessionState
    /// Accept one certificate, by whole-certificate SHA-256 fingerprint.
    func trustCertificate(serverId: String, fingerprint: String) async throws
    /// Drop every certificate accepted for a server.
    func forgetCertificates(serverId: String) async throws
    /// The fingerprints currently accepted for a server.
    func trustedCertificates(serverId: String) throws -> [String]
}

/// Browsing the catalogue.
public protocol CatalogRepository: Sendable {
    /// One page of the catalogue.
    func browse(query: BrowseQuery) async throws -> MediaPage
    /// One title, with its seasons and episodes where it is a show.
    func detail(mediaId: String) async throws -> MediaDetail
    /// Every genre present in the library, for the filter control.
    func genres() async throws -> [String]
    /// The next episode worth playing after `currentEpisodeId`.
    func upNext(showId: String, currentEpisodeId: String) async throws -> EpisodeSummary?
    /// Every library.
    func libraries() async throws -> [LibrarySummary]
    /// One library.
    func library(id libraryId: String) async throws -> LibrarySummary
    /// The files indexed into one library.
    func libraryFiles(libraryId: String) async throws -> [LibraryFileSummary]
}

/// Choosing what to play, playing it, and recording that it was played.
public protocol PlaybackRepository: Sendable {
    /// Tell the core what this device can decode. Called once at launch and
    /// again whenever the software-decode preference changes.
    func setDeviceProfile(_ profile: DeviceProfile)
    /// Every file behind a title, playable or not.
    func sources(mediaId: String) async throws -> [MediaSourceView]
    /// The core's pick among them, with the reasons the rest were rejected.
    func selectSource(mediaId: String, policy: QualityPolicy) async throws -> SourceSelection
    /// The URL, credential and pinned certificates the player needs.
    func playbackConfig(fileId: String) throws -> PlaybackHttpConfig
    /// The same, for a whole server rather than one file. Downloads need this.
    func serverHTTPConfig() throws -> ServerHttpConfig
    /// Report a playback position. The core throttles and queues.
    func reportProgress(
        fileId: String,
        positionSeconds: Double,
        durationSeconds: Double?,
        force: Bool
    ) async throws -> ProgressOutcome
    /// Send whatever the queue is holding.
    func flushProgress() async throws -> UInt32
    /// How much the queue is holding.
    func pendingProgressCount() async throws -> UInt32
    /// Titles this person is part way through.
    func continueWatching(limit: UInt32?) async throws -> [ContinueWatchingEntry]
    /// One page of watch history.
    func history(limit: UInt32?, offset: UInt32?) async throws -> HistoryPage
}

/// The signed-in person's own sessions across their devices.
public protocol SessionRepository: Sendable {
    /// Every active session.
    func sessions() async throws -> [DeviceSession]
    /// End one session, on any device.
    func revoke(sessionId: String) async throws
    /// End every session, including this one.
    func logoutEverywhere() async throws
}

/// Operator actions. Guarded by the server, not by hiding the screen: a
/// non-administrator gets a 403, which surfaces as `BeamFailure.isForbidden`.
public protocol AdminRepository: Sendable {
    /// The dashboard snapshot.
    func status() async throws -> AdminStatus
    /// One page of user accounts.
    func users(limit: UInt32?, offset: UInt32?) async throws -> AdminUserPage
    /// Disable or re-enable an account.
    func setUserDisabled(userId: String, disabled: Bool) async throws
    /// One page of the operational log.
    func logs(limit: UInt32?, offset: UInt32?) async throws -> [AdminLogEntry]
    /// How many log lines the server holds.
    func logCount() async throws -> UInt64
    /// Recent server events.
    func events(limit: UInt32?) async throws -> [AdminEvent]
    /// Add a library rooted at a path on the server.
    func createLibrary(name: String, rootPath: String) async throws -> LibrarySummary
    /// Remove a library.
    func deleteLibrary(id libraryId: String) async throws
    /// Scan a library, returning how many files were added.
    func scanLibrary(id libraryId: String) async throws -> UInt32
    /// Re-run enrichment for one title.
    func refreshMetadata(mediaId: String) async throws
    /// The server's own health report.
    func health() async throws -> ServerHealth
}

/// Local settings, which never leave the device.
public protocol PreferencesRepository: Sendable {
    /// The current preferences.
    func load() -> UserPreferences
    /// Replace them.
    func save(_ preferences: UserPreferences)
}
