import BeamCore
import BeamFFI
import BeamPlayback
import BeamUI
import Foundation

/// Fetches artwork from Beam with the session cookie and the viewer's trust
/// decision attached.
///
/// Beam serves poster and backdrop art itself rather than handing out a
/// provider CDN link (ADR-0015), which changes artwork from an anonymous
/// public fetch into a first-party authenticated one. `AsyncImage` cannot make
/// that request: it attaches no credential, and it cannot accept the
/// self-signed certificate a LAN server presents -- which would fail artwork on
/// exactly the servers the trust prompt exists for.
///
/// Built here rather than in `BeamUI` because this is the one module that sees
/// both the view layer and the playback layer's trust machinery, and it is
/// already where the app is composed.
///
/// The session is built from `serverHTTPConfig()`, not from any one image:
/// the credential and the trust decision are properties of the *server*, and
/// a session built from whichever image loaded first would apply that server's
/// fingerprints to every later one. That is the same reasoning
/// `DownloadCoordinator` records for its background session, and it is rebuilt
/// when the active server changes rather than captured once at launch.
///
/// There is no cache here on purpose. Beam answers artwork with
/// `Cache-Control: public, max-age=86400` and a strong `ETag`, so `URLCache`
/// stores and revalidates it correctly without a second cache to keep in step.
public final class AuthenticatedArtworkLoader: ArtworkLoading, @unchecked Sendable {
    private let configure: @Sendable () -> ServerHttpConfig?
    private let lock = NSLock()
    private var session: URLSession?
    private var headers: [String: String] = [:]
    private var builtFor: String?

    /// Load artwork for whichever server `playback` currently reports.
    public init(playback: any PlaybackRepository) {
        // Captured strongly: `PlaybackRepository` is `Sendable` rather than
        // class-bound, so it cannot be captured weakly, and the repository does
        // not hold the loader, so there is no cycle to break. This mirrors
        // `DownloadCoordinator`, which reads the same config the same way.
        self.configure = { try? playback.serverHTTPConfig() }
    }

    public func data(for url: URL) async -> Data? {
        guard let (session, headers) = currentSession() else { return nil }

        var request = URLRequest(url: url)
        for (field, value) in headers {
            request.setValue(value, forHTTPHeaderField: field)
        }

        guard let (data, response) = try? await session.data(for: request),
            let http = response as? HTTPURLResponse,
            (200..<300).contains(http.statusCode)
        else {
            return nil
        }
        return data
    }

    /// The session for the active server, rebuilt if that server changed.
    private func currentSession() -> (URLSession, [String: String])? {
        lock.lock()
        defer { lock.unlock() }

        guard let config = configure() else { return nil }
        // Fingerprints as well as host: accepting a *new* certificate for the
        // same host has to take effect without restarting the app.
        let fingerprints = config.trustedFingerprints.sorted().joined(separator: ",")
        let identity = config.host + "|" + fingerprints

        if let session, builtFor == identity {
            return (session, headers)
        }

        let configuration = URLSessionConfiguration.default
        configuration.httpShouldSetCookies = false
        configuration.urlCache = URLCache(
            memoryCapacity: 32 * 1024 * 1024,
            diskCapacity: 256 * 1024 * 1024,
            diskPath: "beam-artwork"
        )
        configuration.requestCachePolicy = .useProtocolCachePolicy

        let created = URLSession(
            configuration: configuration,
            delegate: TrustingSessionDelegate(
                evaluator: CertificateTrustEvaluator(
                    trustedFingerprints: config.trustedFingerprints,
                    pinnedHost: config.host
                )
            ),
            delegateQueue: nil
        )

        session?.finishTasksAndInvalidate()
        session = created
        headers = config.headers
        builtFor = identity
        return (created, config.headers)
    }
}
