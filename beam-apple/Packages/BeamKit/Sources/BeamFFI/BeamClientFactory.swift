// The generated vocabulary -- `MediaSummary`, `MediaDetail`, `DeviceProfile`,
// `BeamError` and the rest -- lives in `BeamCoreBindings` and is re-exported
// here, so every target above this one imports `BeamFFI` alone. Reproducing
// those thirty-odd records as parallel Swift structs would be a second
// hand-maintained copy of a generated contract, which is the duplication
// ADR-0012 created the generated client to avoid.
@_exported import BeamCoreBindings
import Foundation

/// Constructs the one `BeamClient` the app uses.
///
/// A single instance is not an optimisation: the client owns the server
/// registry, the session cookie and the durable progress queue, and two of
/// them would mean two registries racing over one storage key. Mirrors the
/// Hilt singleton binding in `beam-android`'s `FfiModule.kt`.
///
/// This is the only place in the app that constructs a `BeamClient`. That rule
/// is architectural rather than hermetic, and the difference from Android is
/// worth stating: on Android, keeping the generated bindings inside `core:ffi`
/// keeps JNA and the `.so` off the unit-test classpath entirely. SwiftPM links
/// a binary target transitively, so no import discipline can reproduce that --
/// what the boundary buys here is a single construction site and a single
/// place to look when the FFI surface changes, not a smaller test dependency.
public enum BeamClientFactory {
    /// Build a client backed by this device's Keychain and Application
    /// Support directory.
    ///
    /// - Parameter store: an alternative persistence boundary, for tests.
    public static func make(store: KeyValueStore? = nil) -> BeamClient {
        BeamClient(storage: store ?? KeychainKeyValueStore())
    }
}
