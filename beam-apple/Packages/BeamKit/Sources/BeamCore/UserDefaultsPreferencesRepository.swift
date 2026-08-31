import BeamModel
import Foundation

/// Preferences in `UserDefaults`.
///
/// Deliberately not in the core's `KeyValueStore`: these never leave the
/// device, are not part of any server's state, and putting them there would
/// mean a settings change had to cross the FFI boundary to be read back by a
/// toggle that is about to redraw.
public struct UserDefaultsPreferencesRepository: PreferencesRepository {
    private static let key = "beam.preferences"
    // `UserDefaults` is documented as thread-safe but predates `Sendable` and
    // is not annotated. `nonisolated(unsafe)` records that this is a deliberate
    // reading of Apple's documentation rather than an unexamined suppression.
    private nonisolated(unsafe) let defaults: UserDefaults

    /// Wrap a defaults store.
    ///
    /// - Parameter defaults: injected so a test gets its own suite rather than
    ///   writing into whatever the simulator happens to be holding.
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load() -> UserPreferences {
        guard let data = defaults.data(forKey: Self.key),
            let preferences = try? JSONDecoder().decode(UserPreferences.self, from: data)
        else {
            // A stored value written by an older build that no longer decodes
            // reads as "no preference set" rather than as an error: losing a
            // toggle is recoverable, refusing to launch is not.
            return .default
        }
        return preferences
    }

    public func save(_ preferences: UserPreferences) {
        guard let data = try? JSONEncoder().encode(preferences) else { return }
        defaults.set(data, forKey: Self.key)
    }
}
