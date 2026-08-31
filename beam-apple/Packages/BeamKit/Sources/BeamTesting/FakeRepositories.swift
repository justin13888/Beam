import BeamCore
import BeamFFI
import BeamModel
import Foundation
import os

/// Stateful fakes, one per repository seam.
///
/// Stateful rather than mocked, deliberately: a screen that adds a server and
/// then lists servers should see it, and only a fake that actually holds state
/// can prove that. A canned-expectation mock would let a screen "pass" while
/// never reading back what it wrote. Mirrors `core/testing/FakeRepositories.kt`.
///
/// These are scaffolding and never the subject of a test. A test that asserts
/// `FakeCatalogRepository` returns what it was configured with proves nothing
/// about the app.

/// State that can be told to fail.
///
/// Every fake carries the same knob, and honouring it is written once here so
/// no fake can silently forget to -- which would leave that repository's error
/// branches untestable while looking as if they were covered.
private protocol FailureHolding {
    var failure: BeamFailure? { get }
}

extension FailureHolding {
    /// Throw the configured failure, if there is one.
    func check() throws {
        if let failure { throw failure }
    }
}

/// A `ServerRepository` that remembers what it was told.
public final class FakeServerRepository: ServerRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State: FailureHolding {
        var servers: [ServerSummary] = []
        var trusted: [String: Set<String>] = [:]
        var failure: BeamFailure?
        var completedCookies: [String: String] = [:]
    }

    /// An empty registry.
    public init(servers: [ServerSummary] = []) {
        state.withLock { $0.servers = servers }
    }

    /// Make every call fail with `failure`, or `nil` to stop failing.
    ///
    /// The knob exists so the error branches -- an unreachable server, an
    /// expired session, a locked keystore -- are reachable from a test rather
    /// than only from a broken network (NFR-205).
    public func fail(with failure: BeamFailure?) {
        state.withLock { $0.failure = failure }
    }

    /// The cookie `completeLogin` was handed, if any.
    public func capturedCookie(serverId: String) -> String? {
        state.withLock { $0.completedCookies[serverId] }
    }

    public func restore() async throws -> [ServerSummary] {
        try state.withLock {
            try $0.check(); return $0.servers
        }
    }

    public func listServers() throws -> [ServerSummary] {
        try state.withLock {
            try $0.check(); return $0.servers
        }
    }

    public func addServer(baseURL: String, displayName: String?) async throws -> ServerSummary {
        try state.withLock { state in
            try state.check()
            if let existing = state.servers.first(where: { $0.baseUrl == baseURL }) {
                return existing
            }
            let summary = ServerSummary(
                id: "server-\(state.servers.count + 1)",
                displayName: displayName ?? baseURL,
                baseUrl: baseURL,
                state: .loggedOut,
                isActive: state.servers.isEmpty
            )
            state.servers.append(summary)
            return summary
        }
    }

    public func selectServer(id serverId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.servers = state.servers.map { server in
                ServerSummary(
                    id: server.id,
                    displayName: server.displayName,
                    baseUrl: server.baseUrl,
                    state: server.state,
                    isActive: server.id == serverId
                )
            }
        }
    }

    public func removeServer(id serverId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.servers.removeAll { $0.id == serverId }
            state.trusted.removeValue(forKey: serverId)
        }
    }

    public func loginURL(serverId: String) throws -> String {
        try state.withLock { state in
            try state.check()
            guard let server = state.servers.first(where: { $0.id == serverId }) else {
                throw BeamFailure(message: "unknown server", requiresSignIn: true)
            }
            return "\(server.baseUrl)/v1/auth/login?redirect=/"
        }
    }

    public func completeLogin(serverId: String, sessionCookie: String) async throws -> UserSummary {
        try state.withLock { state in
            try state.check()
            state.completedCookies[serverId] = sessionCookie
            let user = UserSummary(
                id: "user-1",
                displayName: "Viewer",
                email: "viewer@example.invalid",
                isAdmin: false,
                avatarUrl: nil
            )
            state.servers = state.servers.map { server in
                guard server.id == serverId else { return server }
                return ServerSummary(
                    id: server.id,
                    displayName: server.displayName,
                    baseUrl: server.baseUrl,
                    state: .authenticated(user: user),
                    isActive: server.isActive
                )
            }
            return user
        }
    }

    public func logout(serverId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.servers = state.servers.map { server in
                guard server.id == serverId else { return server }
                return ServerSummary(
                    id: server.id,
                    displayName: server.displayName,
                    baseUrl: server.baseUrl,
                    state: .loggedOut,
                    isActive: server.isActive
                )
            }
        }
    }

    public func sessionState(serverId: String) throws -> SessionState {
        try state.withLock { state in
            try state.check()
            return state.servers.first { $0.id == serverId }?.state ?? .loggedOut
        }
    }

    public func trustCertificate(serverId: String, fingerprint: String) async throws {
        try state.withLock { state in
            try state.check()
            state.trusted[serverId, default: []].insert(fingerprint)
        }
    }

    public func forgetCertificates(serverId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.trusted[serverId] = []
        }
    }

    public func trustedCertificates(serverId: String) throws -> [String] {
        try state.withLock { state in
            try state.check()
            return Array(state.trusted[serverId] ?? [])
        }
    }
}

/// A `CatalogRepository` over an in-memory catalogue.
public final class FakeCatalogRepository: CatalogRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State: FailureHolding {
        var items: [MediaSummary] = []
        var details: [String: MediaDetail] = [:]
        var libraries: [LibrarySummary] = []
        var files: [String: [LibraryFileSummary]] = [:]
        var genres: [String] = []
        var upNext: EpisodeSummary?
        var failure: BeamFailure?
        var lastQuery: BrowseQuery?
    }

    /// A catalogue holding `items`.
    public init(items: [MediaSummary] = [], libraries: [LibrarySummary] = []) {
        state.withLock {
            $0.items = items
            $0.libraries = libraries
            $0.genres = Array(Set(items.flatMap(\.genres))).sorted()
        }
    }

    /// Make every call fail.
    public func fail(with failure: BeamFailure?) {
        state.withLock { $0.failure = failure }
    }

    /// Provide the detail returned for `mediaId`.
    public func setDetail(_ detail: MediaDetail, for mediaId: String) {
        state.withLock { $0.details[mediaId] = detail }
    }

    /// Provide the next episode `upNext` should resolve to.
    public func setUpNext(_ episode: EpisodeSummary?) {
        state.withLock { $0.upNext = episode }
    }

    /// The query the last `browse` was given.
    ///
    /// Lets a test assert what the screen actually asked for -- the debounce
    /// interval, the page size, the filters -- rather than only what came back.
    public func lastQuery() -> BrowseQuery? {
        state.withLock { $0.lastQuery }
    }

    public func browse(query: BrowseQuery) async throws -> MediaPage {
        try state.withLock { state in
            try state.check()
            state.lastQuery = query
            var items = state.items
            if let search = query.query, !search.isEmpty {
                items = items.filter { $0.title.localizedCaseInsensitiveContains(search) }
            }
            if let genre = query.genre {
                items = items.filter { $0.genres.contains(genre) }
            }
            return Fixtures.page(items)
        }
    }

    public func detail(mediaId: String) async throws -> MediaDetail {
        try state.withLock { state in
            try state.check()
            guard let detail = state.details[mediaId] else {
                throw BeamFailure(message: "not found")
            }
            return detail
        }
    }

    public func genres() async throws -> [String] {
        try state.withLock {
            try $0.check(); return $0.genres
        }
    }

    public func upNext(showId: String, currentEpisodeId: String) async throws -> EpisodeSummary? {
        try state.withLock {
            try $0.check(); return $0.upNext
        }
    }

    public func libraries() async throws -> [LibrarySummary] {
        try state.withLock {
            try $0.check(); return $0.libraries
        }
    }

    public func library(id libraryId: String) async throws -> LibrarySummary {
        try state.withLock { state in
            try state.check()
            guard let library = state.libraries.first(where: { $0.id == libraryId }) else {
                throw BeamFailure(message: "not found")
            }
            return library
        }
    }

    public func libraryFiles(libraryId: String) async throws -> [LibraryFileSummary] {
        try state.withLock {
            try $0.check(); return $0.files[libraryId] ?? []
        }
    }
}

/// A `PlaybackRepository` that records what it was told.
public final class FakePlaybackRepository: PlaybackRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State: FailureHolding {
        var sources: [String: [MediaSourceView]] = [:]
        var selection: SourceSelection?
        var continueWatching: [ContinueWatchingEntry] = []
        var history: [HistoryEntry] = []
        var reports: [(fileId: String, position: Double, forced: Bool)] = []
        var profile: DeviceProfile?
        var failure: BeamFailure?
    }

    /// A repository with nothing in it.
    public init(
        continueWatching: [ContinueWatchingEntry] = [],
        history: [HistoryEntry] = []
    ) {
        state.withLock {
            $0.continueWatching = continueWatching
            $0.history = history
        }
    }

    /// Make every call fail.
    public func fail(with failure: BeamFailure?) {
        state.withLock { $0.failure = failure }
    }

    /// Provide the sources returned for `mediaId`.
    public func setSources(_ sources: [MediaSourceView], for mediaId: String) {
        state.withLock { $0.sources[mediaId] = sources }
    }

    /// Provide the selection `selectSource` should return.
    public func setSelection(_ selection: SourceSelection?) {
        state.withLock { $0.selection = selection }
    }

    /// Every progress report made, in order.
    public func reports() -> [(fileId: String, position: Double, forced: Bool)] {
        state.withLock { $0.reports }
    }

    /// The profile the app pushed at launch.
    public func deviceProfile() -> DeviceProfile? {
        state.withLock { $0.profile }
    }

    public func setDeviceProfile(_ profile: DeviceProfile) {
        state.withLock { $0.profile = profile }
    }

    public func sources(mediaId: String) async throws -> [MediaSourceView] {
        try state.withLock {
            try $0.check(); return $0.sources[mediaId] ?? []
        }
    }

    public func selectSource(mediaId: String, policy: QualityPolicy) async throws -> SourceSelection
    {
        try state.withLock { state in
            try state.check()
            guard let selection = state.selection else {
                throw BeamFailure(message: "no playable source")
            }
            return selection
        }
    }

    public func playbackConfig(fileId: String) throws -> PlaybackHttpConfig {
        try state.withLock {
            try $0.check(); return Fixtures.playbackConfig(fileId: fileId)
        }
    }

    public func serverHTTPConfig() throws -> ServerHttpConfig {
        try state.withLock { state in
            try state.check()
            return ServerHttpConfig(
                baseUrl: "https://beam.invalid",
                headers: ["Cookie": "beam_session=opaque"],
                trustedFingerprints: [],
                host: "beam.invalid"
            )
        }
    }

    public func reportProgress(
        fileId: String,
        positionSeconds: Double,
        durationSeconds: Double?,
        force: Bool
    ) async throws -> ProgressOutcome {
        try state.withLock { state in
            try state.check()
            state.reports.append((fileId, positionSeconds, force))
            return .sent(positionSecs: positionSeconds)
        }
    }

    public func flushProgress() async throws -> UInt32 {
        try state.withLock {
            try $0.check(); return 0
        }
    }

    public func pendingProgressCount() async throws -> UInt32 {
        try state.withLock {
            try $0.check(); return 0
        }
    }

    public func continueWatching(limit: UInt32?) async throws -> [ContinueWatchingEntry] {
        try state.withLock {
            try $0.check(); return $0.continueWatching
        }
    }

    public func history(limit: UInt32?, offset: UInt32?) async throws -> HistoryPage {
        try state.withLock { state in
            try state.check()
            let start = Int(offset ?? 0)
            let end = min(state.history.count, start + Int(limit ?? 50))
            let slice = start < end ? Array(state.history[start..<end]) : []
            return HistoryPage(items: slice, total: UInt64(state.history.count))
        }
    }
}

/// A `SessionRepository` over an in-memory list.
public final class FakeSessionRepository: SessionRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State: FailureHolding {
        var sessions: [DeviceSession] = []
        var failure: BeamFailure?
    }

    /// A repository holding `sessions`.
    public init(sessions: [DeviceSession] = []) {
        state.withLock { $0.sessions = sessions }
    }

    /// Make every call fail.
    public func fail(with failure: BeamFailure?) {
        state.withLock { $0.failure = failure }
    }

    public func sessions() async throws -> [DeviceSession] {
        try state.withLock {
            try $0.check(); return $0.sessions
        }
    }

    public func revoke(sessionId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.sessions.removeAll { $0.id == sessionId }
        }
    }

    public func logoutEverywhere() async throws {
        try state.withLock { state in
            try state.check()
            state.sessions = []
        }
    }
}

/// An `AdminRepository` over in-memory state.
public final class FakeAdminRepository: AdminRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State: FailureHolding {
        var status: AdminStatus?
        var users: [AdminUser] = []
        var logs: [AdminLogEntry] = []
        var events: [AdminEvent] = []
        var libraries: [LibrarySummary] = []
        var scanned: [String] = []
        var failure: BeamFailure?
    }

    /// A repository with nothing in it.
    public init(status: AdminStatus? = nil, users: [AdminUser] = []) {
        state.withLock {
            $0.status = status
            $0.users = users
        }
    }

    /// Make every call fail. A `.isForbidden` failure is how a
    /// non-administrator is simulated, matching the server's own 403.
    public func fail(with failure: BeamFailure?) {
        state.withLock { $0.failure = failure }
    }

    /// Which libraries were scanned, in order.
    public func scannedLibraries() -> [String] {
        state.withLock { $0.scanned }
    }

    public func status() async throws -> AdminStatus {
        try state.withLock { state in
            try state.check()
            guard let status = state.status else { throw BeamFailure(message: "no status") }
            return status
        }
    }

    public func users(limit: UInt32?, offset: UInt32?) async throws -> AdminUserPage {
        try state.withLock { state in
            try state.check()
            return AdminUserPage(items: state.users, total: UInt64(state.users.count))
        }
    }

    public func setUserDisabled(userId: String, disabled: Bool) async throws {
        try state.withLock { (state: inout State) in
            try state.check()
            state.users = state.users.map { user in
                guard user.id == userId else { return user }
                return AdminUser(
                    id: user.id,
                    displayName: user.displayName,
                    email: user.email,
                    avatarUrl: user.avatarUrl,
                    isAdmin: user.isAdmin,
                    disabled: disabled,
                    createdAtUnix: user.createdAtUnix
                )
            }
        }
    }

    public func logs(limit: UInt32?, offset: UInt32?) async throws -> [AdminLogEntry] {
        try state.withLock {
            try $0.check(); return $0.logs
        }
    }

    public func logCount() async throws -> UInt64 {
        try state.withLock {
            try $0.check(); return UInt64($0.logs.count)
        }
    }

    public func events(limit: UInt32?) async throws -> [AdminEvent] {
        try state.withLock {
            try $0.check(); return $0.events
        }
    }

    public func createLibrary(name: String, rootPath: String) async throws -> LibrarySummary {
        try state.withLock { state in
            try state.check()
            let library = Fixtures.library(
                id: "library-\(state.libraries.count + 1)",
                name: name,
                size: 0
            )
            state.libraries.append(library)
            return library
        }
    }

    public func deleteLibrary(id libraryId: String) async throws {
        try state.withLock { state in
            try state.check()
            state.libraries.removeAll { $0.id == libraryId }
        }
    }

    public func scanLibrary(id libraryId: String) async throws -> UInt32 {
        try state.withLock { state in
            try state.check()
            state.scanned.append(libraryId)
            return 7
        }
    }

    public func refreshMetadata(mediaId: String) async throws {
        try state.withLock { try $0.check() }
    }

    public func health() async throws -> ServerHealth {
        try state.withLock { state in
            try state.check()
            return ServerHealth(
                status: "ok",
                version: "0.1.0",
                uptimeSecs: 3600,
                database: "ok"
            )
        }
    }
}

/// A `PreferencesRepository` that holds preferences in memory.
public final class FakePreferencesRepository: PreferencesRepository, @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: UserPreferences.default)

    /// A repository starting from `preferences`.
    public init(_ preferences: UserPreferences = .default) {
        state.withLock { $0 = preferences }
    }

    public func load() -> UserPreferences {
        state.withLock { $0 }
    }

    public func save(_ preferences: UserPreferences) {
        state.withLock { $0 = preferences }
    }
}
