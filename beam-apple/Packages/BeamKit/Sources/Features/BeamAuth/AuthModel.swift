import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// Connecting to a server and signing in.
///
/// The flow is a web view whose cookie jar is read, which is a liability
/// recorded as such (NFR-605, ADR-0012) rather than a design. `beam-server`
/// reads exactly one credential -- the `beam_session` cookie -- and its
/// redirect sanitizer accepts only same-origin relative paths, so the OIDC
/// provider cannot redirect to a custom scheme a native app could intercept.
/// `ASWebAuthenticationSession` would be the right tool and cannot be used: its
/// cookie jar is not readable by the app, which would leave the credential
/// somewhere the app can never see it.
///
/// The seam is here rather than in the screens, so a native token mint can
/// replace this without any screen changing.
@MainActor
@Observable
public final class AuthModel {
    /// What the sign-in screen is doing.
    public enum Phase: Equatable {
        /// Waiting for a server address.
        case idle
        /// Reaching the server.
        case connecting
        /// Showing the web view at this URL.
        case signingIn(url: URL, serverId: String)
        /// Signed in.
        case signedIn
        /// Stopped, with a reason.
        case failed(String)
    }

    /// Where the flow has got to.
    public private(set) var phase: Phase = .idle
    /// Servers already set up on this device.
    public private(set) var servers: [ServerSummary] = []
    /// A certificate awaiting a decision, with the host that presented it.
    public private(set) var pendingTrust: (host: String, details: CertificateDetails)?

    /// The address being typed.
    public var address = ""

    @ObservationIgnored private let registry: any ServerRepository
    @ObservationIgnored private var pendingAddress: String?

    /// Build a model over the server seam.
    public init(servers: any ServerRepository) {
        self.registry = servers
    }

    /// Load the registry.
    public func load() async {
        servers = (try? await registry.restore()) ?? []
    }

    /// Add the typed address and begin signing in.
    public func connect() async {
        let trimmed = address.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        pendingAddress = trimmed
        await connect(to: trimmed)
    }

    /// Sign in to a server already in the registry.
    public func signIn(to server: ServerSummary) async {
        pendingAddress = server.baseUrl
        await beginSignIn(serverId: server.id)
    }

    /// Accept the certificate currently awaiting a decision, and retry.
    ///
    /// The digest shown is the whole-certificate SHA-256, the value
    /// `openssl x509 -fingerprint -sha256` prints -- a trust decision the user
    /// cannot independently verify is theatre.
    public func acceptPendingCertificate() async {
        guard let pending = pendingTrust, let address = pendingAddress else { return }
        pendingTrust = nil
        // The server has to exist before a certificate can be pinned against
        // it, and it does: the failure came from a call made after it was
        // added.
        if let server = servers.first(where: { $0.baseUrl == address }) {
            try? await registry.trustCertificate(
                serverId: server.id,
                fingerprint: pending.details.sha256Fingerprint
            )
        }
        await connect(to: address)
    }

    /// Dismiss the trust prompt without accepting.
    public func rejectPendingCertificate() {
        pendingTrust = nil
        phase = .failed("The certificate was not trusted, so Beam did not connect.")
    }

    /// Hand a cookie lifted from the web view to the core.
    public func completeSignIn(serverId: String, cookie: String) async {
        do {
            _ = try await registry.completeLogin(serverId: serverId, sessionCookie: cookie)
            servers = (try? registry.listServers()) ?? servers
            phase = .signedIn
        } catch {
            phase = .failed(BeamFailure.from(error).message)
        }
    }

    /// Abandon a sign-in in progress.
    public func cancelSignIn() {
        phase = .idle
    }

    private func connect(to address: String) async {
        phase = .connecting
        do {
            let summary = try await registry.addServer(baseURL: address, displayName: nil)
            try await registry.selectServer(id: summary.id)
            servers = (try? registry.listServers()) ?? servers
            await beginSignIn(serverId: summary.id)
        } catch {
            handle(error)
        }
    }

    private func beginSignIn(serverId: String) async {
        do {
            try await registry.selectServer(id: serverId)
            let urlString = try registry.loginURL(serverId: serverId)
            guard let url = URL(string: urlString) else {
                phase = .failed("The server returned an unusable sign-in address.")
                return
            }
            phase = .signingIn(url: url, serverId: serverId)
        } catch {
            handle(error)
        }
    }

    private func handle(_ error: Error) {
        let failure = BeamFailure.from(error)
        if let certificate = failure.untrustedCertificate, let host = failure.untrustedHost {
            pendingTrust = (host, certificate)
            phase = .idle
            return
        }
        phase = .failed(failure.message)
    }
}
