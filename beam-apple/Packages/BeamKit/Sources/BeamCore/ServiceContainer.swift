import BeamFFI
import BeamModel
import Foundation

/// Everything a screen might need, resolved once at launch.
///
/// Swift has no Hilt, and a hand-rolled graph would be ceremony for six
/// objects with no cycles. This is the whole of the app's dependency
/// injection: constructed once in the app target, handed down through the
/// SwiftUI environment, and replaced wholesale by `BeamTesting` in a test.
@MainActor
@Observable
public final class ServiceContainer {
    /// The server registry and session.
    public let servers: any ServerRepository
    /// Browsing.
    public let catalog: any CatalogRepository
    /// Playback selection and progress.
    public let playback: any PlaybackRepository
    /// This person's sessions.
    public let sessions: any SessionRepository
    /// Operator actions.
    public let admin: any AdminRepository
    /// Local settings.
    public let preferences: any PreferencesRepository

    /// The preferences currently in force, observable so a theme change
    /// redraws without every screen subscribing separately.
    public private(set) var currentPreferences: UserPreferences

    /// Build a container over a set of repositories.
    public init(
        servers: any ServerRepository,
        catalog: any CatalogRepository,
        playback: any PlaybackRepository,
        sessions: any SessionRepository,
        admin: any AdminRepository,
        preferences: any PreferencesRepository
    ) {
        self.servers = servers
        self.catalog = catalog
        self.playback = playback
        self.sessions = sessions
        self.admin = admin
        self.preferences = preferences
        self.currentPreferences = preferences.load()
    }

    /// The production graph, over one `BeamClient`.
    public static func live() -> ServiceContainer {
        let repositories = BeamRepositories(client: BeamClientFactory.make())
        return ServiceContainer(
            servers: repositories,
            catalog: repositories,
            playback: repositories,
            sessions: repositories,
            admin: repositories,
            preferences: UserDefaultsPreferencesRepository()
        )
    }

    /// Persist a change and republish it.
    public func update(_ preferences: UserPreferences) {
        currentPreferences = preferences
        self.preferences.save(preferences)
    }
}
