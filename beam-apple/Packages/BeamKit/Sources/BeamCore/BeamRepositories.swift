import BeamFFI
import BeamModel
import Foundation

/// Every repository, over one `BeamClient`.
///
/// One type rather than six, because the six protocols are six views of one
/// object and splitting the adapter would mean six copies of the same
/// `mapping` helper. The protocols stay separate so a screen declares only the
/// surface it uses and a fake only has to implement that much.
public struct BeamRepositories: ServerRepository, CatalogRepository, PlaybackRepository,
    SessionRepository, AdminRepository
{
    private let client: BeamClient

    /// Wrap a client.
    public init(client: BeamClient) {
        self.client = client
    }

    // MARK: - ServerRepository

    public func restore() async throws -> [ServerSummary] {
        try await mapping { try await client.restore() }
    }

    public func listServers() throws -> [ServerSummary] {
        try mappingSync { try client.listServers() }
    }

    public func addServer(baseURL: String, displayName: String?) async throws -> ServerSummary {
        try await mapping { try await client.addServer(baseUrl: baseURL, displayName: displayName) }
    }

    public func selectServer(id serverId: String) async throws {
        try await mapping { try await client.selectServer(serverId: serverId) }
    }

    public func removeServer(id serverId: String) async throws {
        try await mapping { try await client.removeServer(serverId: serverId) }
    }

    public func loginURL(serverId: String) throws -> String {
        try mappingSync { try client.loginUrl(serverId: serverId) }
    }

    public func completeLogin(serverId: String, sessionCookie: String) async throws -> UserSummary {
        try await mapping {
            try await client.completeLogin(serverId: serverId, sessionCookie: sessionCookie)
        }
    }

    public func logout(serverId: String) async throws {
        try await mapping { try await client.logout(serverId: serverId) }
    }

    public func sessionState(serverId: String) throws -> SessionState {
        try mappingSync { try client.sessionState(serverId: serverId) }
    }

    public func trustCertificate(serverId: String, fingerprint: String) async throws {
        try await mapping {
            try await client.trustCertificate(serverId: serverId, fingerprint: fingerprint)
        }
    }

    public func forgetCertificates(serverId: String) async throws {
        try await mapping { try await client.forgetCertificates(serverId: serverId) }
    }

    public func trustedCertificates(serverId: String) throws -> [String] {
        try mappingSync { try client.trustedCertificates(serverId: serverId) }
    }

    // MARK: - CatalogRepository

    public func browse(query: BrowseQuery) async throws -> MediaPage {
        try await mapping { try await client.browseMedia(query: query) }
    }

    public func detail(mediaId: String) async throws -> MediaDetail {
        try await mapping { try await client.mediaDetail(mediaId: mediaId) }
    }

    public func genres() async throws -> [String] {
        try await mapping { try await client.genres() }
    }

    public func upNext(showId: String, currentEpisodeId: String) async throws -> EpisodeSummary? {
        try await mapping {
            try await client.upNextInShow(showId: showId, currentEpisodeId: currentEpisodeId)
        }
    }

    public func libraries() async throws -> [LibrarySummary] {
        try await mapping { try await client.libraries() }
    }

    public func library(id libraryId: String) async throws -> LibrarySummary {
        try await mapping { try await client.library(libraryId: libraryId) }
    }

    public func libraryFiles(libraryId: String) async throws -> [LibraryFileSummary] {
        try await mapping { try await client.libraryFiles(libraryId: libraryId) }
    }

    // MARK: - PlaybackRepository

    public func setDeviceProfile(_ profile: DeviceProfile) {
        client.setDeviceProfile(profile: profile)
    }

    public func sources(mediaId: String) async throws -> [MediaSourceView] {
        try await mapping { try await client.mediaSources(mediaId: mediaId) }
    }

    public func selectSource(mediaId: String, policy: QualityPolicy) async throws -> SourceSelection
    {
        try await mapping {
            try await client.selectPlaybackSource(mediaId: mediaId, policy: policy)
        }
    }

    public func playbackConfig(fileId: String) throws -> PlaybackHttpConfig {
        try mappingSync { try client.playbackConfig(fileId: fileId) }
    }

    public func serverHTTPConfig() throws -> ServerHttpConfig {
        try mappingSync { try client.serverHttpConfig() }
    }

    public func reportProgress(
        fileId: String,
        positionSeconds: Double,
        durationSeconds: Double?,
        force: Bool
    ) async throws -> ProgressOutcome {
        try await mapping {
            try await client.reportProgress(
                fileId: fileId,
                positionSecs: positionSeconds,
                durationSecs: durationSeconds,
                force: force
            )
        }
    }

    public func flushProgress() async throws -> UInt32 {
        try await mapping { try await client.flushProgress() }
    }

    public func pendingProgressCount() async throws -> UInt32 {
        try await mapping { try await client.pendingProgressCount() }
    }

    public func continueWatching(limit: UInt32?) async throws -> [ContinueWatchingEntry] {
        try await mapping { try await client.continueWatching(limit: limit) }
    }

    public func history(limit: UInt32?, offset: UInt32?) async throws -> HistoryPage {
        try await mapping { try await client.history(limit: limit, offset: offset) }
    }

    // MARK: - SessionRepository

    public func sessions() async throws -> [DeviceSession] {
        try await mapping { try await client.sessions() }
    }

    public func revoke(sessionId: String) async throws {
        try await mapping { try await client.revokeSession(sessionId: sessionId) }
    }

    public func logoutEverywhere() async throws {
        try await mapping { try await client.logoutEverywhere() }
    }

    // MARK: - AdminRepository

    public func status() async throws -> AdminStatus {
        try await mapping { try await client.adminStatus() }
    }

    public func users(limit: UInt32?, offset: UInt32?) async throws -> AdminUserPage {
        try await mapping { try await client.adminUsers(limit: limit, offset: offset) }
    }

    public func setUserDisabled(userId: String, disabled: Bool) async throws {
        try await mapping { try await client.setUserDisabled(userId: userId, disabled: disabled) }
    }

    public func logs(limit: UInt32?, offset: UInt32?) async throws -> [AdminLogEntry] {
        try await mapping { try await client.adminLogs(limit: limit, offset: offset) }
    }

    public func logCount() async throws -> UInt64 {
        try await mapping { try await client.adminLogCount() }
    }

    public func events(limit: UInt32?) async throws -> [AdminEvent] {
        try await mapping { try await client.adminEvents(limit: limit) }
    }

    public func createLibrary(name: String, rootPath: String) async throws -> LibrarySummary {
        try await mapping { try await client.createLibrary(name: name, rootPath: rootPath) }
    }

    public func deleteLibrary(id libraryId: String) async throws {
        try await mapping { try await client.deleteLibrary(libraryId: libraryId) }
    }

    public func scanLibrary(id libraryId: String) async throws -> UInt32 {
        try await mapping { try await client.scanLibrary(libraryId: libraryId) }
    }

    public func refreshMetadata(mediaId: String) async throws {
        try await mapping { try await client.refreshMediaMetadata(mediaId: mediaId) }
    }

    public func health() async throws -> ServerHealth {
        try await mapping { try await client.health() }
    }

    // MARK: - Error mapping

    // Every call goes through one of these two, so a screen can never receive
    // a raw `BeamError`. Doing it at each call site instead would mean one
    // forgotten `catch` is a crash-shaped error message in front of a person.

    private func mapping<T>(_ body: () async throws -> T) async throws -> T {
        do {
            return try await body()
        } catch {
            throw BeamFailure.from(error)
        }
    }

    private func mappingSync<T>(_ body: () throws -> T) throws -> T {
        do {
            return try body()
        } catch {
            throw BeamFailure.from(error)
        }
    }
}
