import Foundation
import SwiftUI

/// Fetches the bytes of one artwork URL.
///
/// A seam rather than a direct `URLSession` call, because artwork now comes
/// from Beam itself (ADR-0015) rather than from a provider CDN, and reaching
/// Beam takes two things this module has no business knowing: the session
/// cookie, and the certificate the viewer accepted for a LAN server with a
/// self-signed one. `BeamAppShell` supplies a loader that has both.
///
/// The default is a plain unauthenticated fetch. That is what previews and
/// snapshot rendering get, and it is deliberately the *old* behaviour: a view
/// rendered outside the app has no session to attach, and should show what it
/// always showed rather than fail.
public protocol ArtworkLoading: Sendable {
    /// The encoded image at `url`, or `nil` if it could not be fetched.
    func data(for url: URL) async -> Data?
}

/// Fetches with no credential and default trust.
public struct UnauthenticatedArtworkLoader: ArtworkLoading {
    public init() {}

    public func data(for url: URL) async -> Data? {
        guard let (data, _) = try? await URLSession.shared.data(from: url) else { return nil }
        return data
    }
}

private struct ArtworkLoaderKey: EnvironmentKey {
    static let defaultValue: any ArtworkLoading = UnauthenticatedArtworkLoader()
}

extension EnvironmentValues {
    /// How ``BeamArtwork`` fetches images.
    public var artworkLoader: any ArtworkLoading {
        get { self[ArtworkLoaderKey.self] }
        set { self[ArtworkLoaderKey.self] = newValue }
    }
}
