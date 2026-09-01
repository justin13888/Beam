import BeamCoreBindings
import Foundation
import Testing

@testable import BeamFFI

/// How a core failure is phrased for a viewer.
///
/// The mapping had no tests at all, which is how it went unnoticed that the
/// core could not actually produce most of the cases it handles: every server
/// failure arrived as `.Network` until the core learned to classify from the
/// response. These cover the distinctions a screen acts on.
@Suite("Beam failure")
struct BeamFailureTests {
    /// Both of these are 404s, so the status cannot separate them.
    ///
    /// That is exactly what the problem type is for. One means the viewer
    /// asked for something that is not there; the other means the catalogue
    /// still lists the title and the server no longer has its file, which
    /// nothing the viewer does will fix.
    @Test("a missing source file is not phrased as the viewer's mistake")
    func missingSourceFileNamesWhoCanFixIt() {
        let absent = BeamFailure.from(
            BeamError.NotFound(
                detail: "No title with id 0f3c",
                code: "https://beam.example/reference/errors/#media-not-found"
            )
        )
        let diverged = BeamFailure.from(
            BeamError.NotFound(
                detail: "Source video file not found",
                code: "https://beam.example/reference/errors/#source-file-missing"
            )
        )

        #expect(!absent.message.contains("administrator"))
        #expect(diverged.message.contains("administrator"))
        #expect(!diverged.isRetryable)
    }

    /// A 403 is not a sign-in prompt: signing in again cannot grant a
    /// permission the account does not have.
    @Test("a forbidden action is marked as such rather than sent to sign-in")
    func forbiddenIsNotASignInPrompt() {
        let failure = BeamFailure.from(
            BeamError.Forbidden(detail: "admin access required", code: "about:blank")
        )

        #expect(failure.isForbidden)
        #expect(!failure.requiresSignIn)
        #expect(!failure.isRetryable)
    }

    /// Retryability is read off the error, not re-derived from the status.
    ///
    /// The core decides it in `classify` and carries it on `Server`, the way
    /// it already did on `Network`. This asserts the mapping reads it both
    /// ways round: deriving it here answered "retryable" for every status,
    /// which offered a retry for a 415 the server refuses identically forever.
    @Test("retryability is carried by the error, not re-derived")
    func retryabilityIsCarriedByTheError() {
        let transient = BeamFailure.from(
            BeamError.Server(
                status: 503, retryable: true, detail: "down", code: "about:blank"
            )
        )
        let refused = BeamFailure.from(
            BeamError.Server(
                status: 415, retryable: false, detail: "no", code: "about:blank"
            )
        )
        let malformed = BeamFailure.from(
            BeamError.BadRequest(detail: "not a valid identifier", code: "about:blank")
        )

        #expect(transient.isRetryable)
        #expect(!refused.isRetryable)
        #expect(!malformed.isRetryable)
    }
}
