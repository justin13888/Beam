import BeamCoreBindings
import Foundation

/// A core failure, in the shape a screen needs to react to it.
///
/// The generated `BeamError` has fifteen cases and a screen cares about four
/// questions: what do I show, do I offer a retry, do I send them to sign in,
/// and is there a certificate to decide about. Flattening it here means those
/// four questions are answered once rather than in ten view models. Mirrors
/// `BeamErrors.kt` in `beam-android`.
public struct BeamFailure: Error, Equatable, Sendable {
    /// A message fit to put in front of a person.
    public let message: String
    /// Whether the same call is worth making again.
    public let isRetryable: Bool
    /// Whether the only way forward is a new sign-in.
    public let requiresSignIn: Bool
    /// Whether this was the server's refusal of an administrative action.
    public let isForbidden: Bool
    /// The certificate awaiting a trust decision, where that is the problem.
    public let untrustedCertificate: CertificateDetails?
    /// The host that presented it. Shown beside the fingerprint, because a
    /// digest with no host is a decision no one can actually make.
    public let untrustedHost: String?

    /// Memberwise.
    public init(
        message: String,
        isRetryable: Bool = false,
        requiresSignIn: Bool = false,
        isForbidden: Bool = false,
        untrustedCertificate: CertificateDetails? = nil,
        untrustedHost: String? = nil
    ) {
        self.message = message
        self.isRetryable = isRetryable
        self.requiresSignIn = requiresSignIn
        self.isForbidden = isForbidden
        self.untrustedCertificate = untrustedCertificate
        self.untrustedHost = untrustedHost
    }

    /// Flatten any error into something a screen can render.
    ///
    /// Takes `Error` rather than `BeamError` so a call site never has to
    /// decide whether a given throw came from the core -- a `catch` that had
    /// to distinguish would eventually get it wrong and drop a real failure.
    public static func from(_ error: Error) -> BeamFailure {
        if let failure = error as? BeamFailure {
            return failure
        }
        guard let beamError = error as? BeamError else {
            return BeamFailure(message: error.localizedDescription)
        }
        return from(beamError)
    }

    /// Flatten a core failure.
    ///
    /// The message is decided here, because it is UI copy. Whether a retry is
    /// honest is decided by the core: `Network` and `Server` both carry the
    /// verdict on the error itself, so this reads it rather than deriving it.
    /// It used to derive it, and answered "retryable" for every `Server`
    /// status -- so a 415 or a 422, which three operations declare, offered a
    /// retry for a body the server refuses identically every time.
    public static func from(_ error: BeamError) -> BeamFailure {
        switch error {
        case .NoActiveServer:
            return BeamFailure(
                message: "No server selected.",
                requiresSignIn: true
            )
        case .UnknownServer:
            return BeamFailure(
                message: "That server is no longer set up on this device.",
                requiresSignIn: true
            )
        case .InvalidServerUrl(let detail):
            return BeamFailure(message: "That address will not work: \(detail)")
        case .Unauthenticated:
            return BeamFailure(message: "Sign in to continue.", requiresSignIn: true)
        case .SessionExpired:
            // Deliberately distinct from Unauthenticated: the screen keeps
            // what the person was doing and asks them back in, rather than
            // resetting to a cold sign-in.
            return BeamFailure(
                message: "Your session has expired. Sign in again to continue.",
                requiresSignIn: true
            )
        case .Forbidden(let detail, _):
            return BeamFailure(message: detail, isForbidden: true)
        case .NotFound:
            return BeamFailure(
                message: "That is no longer in the library. It may have been removed by a scan."
            )
        case .BadRequest(let detail, _):
            return BeamFailure(message: detail)
        case .RateLimited(let retryAfterSecs):
            return BeamFailure(
                message: "Too many requests. Try again in \(retryAfterSecs)s.",
                isRetryable: true
            )
        case .Server(let status, let retryable, _, _):
            return BeamFailure(
                message: "The server had a problem (\(status)). Try again shortly.",
                isRetryable: retryable
            )
        case .Network(let detail, let retryable):
            return BeamFailure(
                message: "Could not reach the server: \(detail)",
                isRetryable: retryable
            )
        case .UntrustedCertificate(let host, let details):
            return BeamFailure(
                message: "\(host) presented a certificate your device does not trust.",
                untrustedCertificate: details,
                untrustedHost: host
            )
        case .Protocol(let detail):
            return BeamFailure(message: "Unexpected response from the server: \(detail)")
        // Retryable, matching `error.rs`: a full disk is emptied and a locked
        // keystore unlocks, so the viewer who frees space has a real path to
        // success. Android said so first; the core now states it too.
        case .Storage(let detail):
            return BeamFailure(
                message: "This device could not save that: \(detail)",
                isRetryable: true
            )
        }
    }
}
