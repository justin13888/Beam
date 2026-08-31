import Foundation
import Testing

@testable import BeamAuth

/// Which cookie the sign-in flow is willing to lift.
///
/// The OIDC exchange redirects through the identity provider's hosts, and any
/// of them is free to set a cookie called `beam_session`. Handing one of those
/// to the core as if it were the server's would be a credential-confusion bug,
/// so the domain check is the security-relevant part of the flow rather than
/// an incidental filter.
@Suite("Cookie lift")
struct CookieLiftTests {
    private func coordinator(host: String) -> SignInCoordinator {
        SignInCoordinator(host: host, onCookie: { _ in })
    }

    @Test("the server's own cookie is accepted")
    func exactHostMatches() {
        let subject = coordinator(host: "beam.example.com")

        #expect(subject.matchesHost("beam.example.com"))
        // A domain-scoped cookie is written with a leading dot.
        #expect(subject.matchesHost(".beam.example.com"))
    }

    @Test("a cookie scoped to a parent domain is accepted")
    func parentDomainMatches() {
        // `Domain=.example.com` legitimately covers `beam.example.com`, which
        // is how a server behind a shared domain sets its session cookie.
        let subject = coordinator(host: "beam.example.com")

        #expect(subject.matchesHost(".example.com"))
        #expect(subject.matchesHost("example.com"))
    }

    @Test("an unrelated host's cookie is refused")
    func unrelatedHostIsRefused() {
        let subject = coordinator(host: "beam.example.com")

        #expect(!subject.matchesHost("login.microsoftonline.com"))
        #expect(!subject.matchesHost("accounts.google.com"))
    }

    @Test("a suffix that is not a domain boundary is refused")
    func suffixIsNotEnough() {
        // "notbeam.example.com" ends with "example.com" and is a different
        // host; matching on a bare string suffix would accept
        // "evilexample.com" for "example.com" too.
        let subject = coordinator(host: "beam.example.com")

        #expect(!subject.matchesHost("m.example.com"))
        #expect(!subject.matchesHost("evilexample.com"))
    }
}
