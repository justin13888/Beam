import BeamFFI
import BeamModel

/// The stored preference, as the core's policy.
///
/// The two types are kept apart on purpose: `UserPreferences` is persisted, and
/// binding a persisted value directly to a generated enum would mean a rename
/// in the OpenAPI contract silently invalidated everyone's settings. Mirrors
/// `QualityPolicies.kt`.
extension QualityPreference {
    /// The policy the core should apply for this preference.
    public var policy: QualityPolicy {
        switch self {
        case .best: .highest
        case .matchScreen: .matchScreen
        case .smallest: .smallest
        }
    }
}
