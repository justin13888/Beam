import CryptoKit
import Foundation
import Security

/// Evaluates a server trust challenge the way the core does.
///
/// A self-hosted server on a LAN routinely presents a self-signed certificate,
/// so "the system trust store said no" cannot be the end of the story -- but
/// nor can it be waved away. This mirrors `beam-client-core`'s `tls.rs`
/// exactly, and the ordering is the whole point:
///
/// 1. The system trust store is consulted first, and **its acceptance is
///    final**. A user pin can never override a rejection the platform did not
///    make, and can never be consulted for a certificate the platform already
///    accepted.
/// 2. Only on failure is a user-accepted digest considered, and only when the
///    whole-certificate SHA-256 matches one the user explicitly accepted for
///    **this host**.
///
/// So a pin widens trust for exactly one certificate on one host. It cannot
/// reject a publicly valid one and it never generalises.
///
/// The digest is the one `openssl x509 -fingerprint -sha256` prints, because a
/// trust decision the user cannot independently verify is theatre.
public struct CertificateTrustEvaluator: Sendable {
    private let trustedFingerprints: Set<String>
    private let pinnedHost: String

    /// Build an evaluator for one host.
    ///
    /// - Parameters:
    ///   - fingerprints: whole-certificate SHA-256 digests, colon-grouped
    ///     uppercase hex, as the core stores and displays them.
    ///   - host: the host those digests apply to.
    public init(trustedFingerprints: [String], pinnedHost: String) {
        self.trustedFingerprints = Set(
            trustedFingerprints.map { $0.uppercased().replacingOccurrences(of: " ", with: "") }
        )
        self.pinnedHost = pinnedHost.lowercased()
    }

    /// Decide a server-trust challenge.
    ///
    /// - Returns: the credential to use, or `nil` to let the challenge fail.
    public func evaluate(_ challenge: URLAuthenticationChallenge) -> URLCredential? {
        guard
            challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
            let trust = challenge.protectionSpace.serverTrust
        else {
            return nil
        }

        // The platform first, and its acceptance is final.
        var error: CFError?
        if SecTrustEvaluateWithError(trust, &error) {
            return URLCredential(trust: trust)
        }

        // Only now is a pin considered, and only for the host it was accepted
        // for. Without this check a certificate trusted for one server would
        // authenticate any server that presented it.
        guard !trustedFingerprints.isEmpty,
            challenge.protectionSpace.host.lowercased() == pinnedHost,
            let leaf = leafCertificate(of: trust),
            trustedFingerprints.contains(Self.fingerprint(of: leaf))
        else {
            return nil
        }

        return URLCredential(trust: trust)
    }

    private func leafCertificate(of trust: SecTrust) -> SecCertificate? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate] else {
            return nil
        }
        return chain.first
    }

    /// The whole-certificate SHA-256, colon-grouped uppercase hex.
    ///
    /// Formatted to match what the core produces and what `openssl` prints, so
    /// the string a user compares against their server is the same string on
    /// both sides.
    public static func fingerprint(of certificate: SecCertificate) -> String {
        let der = SecCertificateCopyData(certificate) as Data
        let digest = SHA256.hash(data: der)
        return digest.map { String(format: "%02X", $0) }.joined(separator: ":")
    }
}

/// A `URLSession` delegate that applies a ``CertificateTrustEvaluator``.
///
/// Shared by the sample-buffer engine's byte source and by downloads, so all
/// three paths -- API, playback and download -- make the same trust decision.
/// A mismatch there does not surface as an authentication error; it surfaces
/// as media that appears corrupt, which is why the core hands the same
/// fingerprints to every one of them.
public final class TrustingSessionDelegate: NSObject, URLSessionDelegate, URLSessionTaskDelegate,
    @unchecked Sendable
{
    private let evaluator: CertificateTrustEvaluator

    /// Wrap an evaluator.
    public init(evaluator: CertificateTrustEvaluator) {
        self.evaluator = evaluator
    }

    public func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard let credential = evaluator.evaluate(challenge) else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        completionHandler(.useCredential, credential)
    }
}
