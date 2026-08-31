import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Preferences, this account's sessions, and the trust store.
@MainActor
@Observable
public final class SettingsModel {
    /// The current preferences.
    public var preferences: UserPreferences {
        didSet {
            guard preferences != oldValue else { return }
            onPreferencesChanged(preferences)
        }
    }

    /// Every active session on this account.
    public private(set) var sessions: LoadState<[DeviceSession]> = .idle
    /// Certificates accepted for the active server.
    public private(set) var trustedFingerprints: [String] = []
    /// The active server.
    public private(set) var activeServer: ServerSummary?
    /// Set when an action failed, for a transient banner.
    public var actionMessage: String?

    @ObservationIgnored private let servers: any ServerRepository
    @ObservationIgnored private let sessionsRepository: any SessionRepository
    @ObservationIgnored private let onPreferencesChanged: (UserPreferences) -> Void

    /// Build a model over the settings seams.
    public init(
        preferences: UserPreferences,
        servers: any ServerRepository,
        sessions: any SessionRepository,
        onPreferencesChanged: @escaping (UserPreferences) -> Void
    ) {
        self.preferences = preferences
        self.servers = servers
        self.sessionsRepository = sessions
        self.onPreferencesChanged = onPreferencesChanged
    }

    /// Load sessions and the trust store.
    public func load() async {
        activeServer = (try? servers.listServers())?.first { $0.isActive }
        if let activeServer {
            trustedFingerprints =
                (try? servers.trustedCertificates(serverId: activeServer.id)) ?? []
        }

        sessions = .loading
        do {
            sessions = .loaded(try await sessionsRepository.sessions())
        } catch {
            sessions = .failed(BeamFailure.from(error).message)
        }
    }

    /// End one session, on any device.
    public func revoke(sessionId: String) async {
        do {
            try await sessionsRepository.revoke(sessionId: sessionId)
            await load()
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// End every session, including this one.
    public func signOutEverywhere() async {
        do {
            try await sessionsRepository.logoutEverywhere()
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// Sign out of the active server on this device only.
    public func signOut() async {
        guard let activeServer else { return }
        do {
            try await servers.logout(serverId: activeServer.id)
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }

    /// Drop every certificate accepted for the active server.
    ///
    /// The next connection will be evaluated against the platform trust store
    /// alone, and will prompt again if it still fails -- which is the point:
    /// a pin the user no longer recognises should be revocable without
    /// removing the server.
    public func forgetCertificates() async {
        guard let activeServer else { return }
        do {
            try await servers.forgetCertificates(serverId: activeServer.id)
            trustedFingerprints = []
        } catch {
            actionMessage = BeamFailure.from(error).message
        }
    }
}
