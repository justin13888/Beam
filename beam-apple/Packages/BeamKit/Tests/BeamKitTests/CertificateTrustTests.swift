import Foundation
import Security
import Testing

@testable import BeamPlayback

/// The digest a user is asked to compare against their server.
///
/// A real certificate, generated with `openssl req -x509`, rather than a
/// hand-built structure -- the same discipline `beam-client-core`'s `tls.rs`
/// applies with `rcgen`. A fabricated input could not disagree with the
/// implementation, which is the entire risk here.
@Suite("Certificate trust")
struct CertificateTrustTests {
    /// A self-signed certificate for `beam.invalid`, DER, base64.
    ///
    /// Regenerate with:
    ///
    ///     openssl req -x509 -newkey rsa:2048 -keyout k.pem -out c.pem \
    ///         -days 3650 -nodes -subj "/CN=beam.invalid" \
    ///         -addext "subjectAltName=DNS:beam.invalid"
    ///     openssl x509 -in c.pem -outform DER -out c.der
    ///     base64 -i c.der | tr -d '\n'
    ///
    /// and take the expected digest from
    /// `openssl x509 -in c.pem -noout -fingerprint -sha256`.
    static let certificateDER =
        "MIIDKDCCAhCgAwIBAgIUIfY3qdyvLJYqh30NFM1EaiqghcgwDQYJKoZIhvcNAQELBQAwFzEVMBMGA1UEAwwMYmVhbS5pbnZhbGlkMB4XDTI2MDgzMTA4NTQwNVoXDTM2MDgyODA4NTQwNVowFzEVMBMGA1UEAwwMYmVhbS5pbnZhbGlkMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA3X5eu0UAQzExnJKr49+SdVmUqutFzRskhcPb/wClTADariJZxk46QFv2xooZEPDck62ZE8maGZ4qkIXceKqKtylt9Wcnr/TmtZgEaXcsAVloxcrKmLE8oCats7hl6cop+1FjpwepNwK0OK5jrYnP7zRKKUwk3mtz6srW/k6OG9HVGXMFY/z7G5wJmyZqd855asWxZwEb4uELD9cCSgyJEqz8WRXhXZBgCgFdCk6XL10Ckq/3K9RCNmnZaud4CpclBciqL505cM76oq73tzkAAPM7mDDa+CvEAfjeA/Ib1UcfbwhfhLGT5xB6kfaR67skHuE97nsW0t4iUleQtV8QqQIDAQABo2wwajAdBgNVHQ4EFgQUvdedaslMH+pwomAMwfGs+TdOwmUwHwYDVR0jBBgwFoAUvdedaslMH+pwomAMwfGs+TdOwmUwDwYDVR0TAQH/BAUwAwEB/zAXBgNVHREEEDAOggxiZWFtLmludmFsaWQwDQYJKoZIhvcNAQELBQADggEBAIUdasi/gvHP1b5QBsbB4KGtpDG+PqNpmiEcHKSiHkMyGEWvJWqUKxzFaYBoAZNUsNpMeQrfpXpl1l1gkDPlTPBZgROGy7TP138qzGJ87ajVSTKM9NQa3nSitWjnN0y0mVtv6mtXVAWSgo80UOFNgEGM32x4lRkXpH1UI68bps2dKxklx8LzAfE4DtoCR9Vi0YLPKnV4O/wk7rESsc1SzqNj8Oy1DLd0bXIC1OPiY5t6u0fL7zrCegn1t0174yR2WQKStjw6dY7A+UMfx4GbvvUzxENyadS4gVi/VoA0TE+p3WPXSZyKYwk1F9axaWnDdQFGqjHeRD8MrKeQh3tF9lw="

    /// What `openssl x509 -fingerprint -sha256` prints for that certificate.
    static let opensslFingerprint =
        "4C:B9:B8:C7:C7:18:06:27:89:5C:39:28:4B:A8:80:66:D1:93:DA:2F:0B:E2:0C:5F:61:EB:DB:98:D3:E9:DE:17"

    @Test("the fingerprint is byte-for-byte what openssl prints")
    func fingerprintMatchesOpenSSL() throws {
        // A trust decision the user cannot independently verify is theatre, so
        // the string Beam shows has to be the string their own tooling shows.
        // This is the assertion that keeps that true.
        let der = try #require(Data(base64Encoded: Self.certificateDER))
        let certificate = try #require(SecCertificateCreateWithData(nil, der as CFData))

        let fingerprint = CertificateTrustEvaluator.fingerprint(of: certificate)

        #expect(fingerprint == Self.opensslFingerprint)
    }

    @Test("the fingerprint is colon-grouped uppercase hex")
    func fingerprintFormatting() throws {
        let der = try #require(Data(base64Encoded: Self.certificateDER))
        let certificate = try #require(SecCertificateCreateWithData(nil, der as CFData))

        let fingerprint = CertificateTrustEvaluator.fingerprint(of: certificate)
        let groups = fingerprint.split(separator: ":")

        #expect(groups.count == 32, "SHA-256 is 32 bytes")
        #expect(groups.allSatisfy { $0.count == 2 })
        #expect(fingerprint == fingerprint.uppercased())
    }
}
