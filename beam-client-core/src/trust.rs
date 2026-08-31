//! What the user is shown before deciding to trust a server certificate.
//!
//! Self-hosted Beam servers routinely run on a LAN behind a self-signed
//! certificate or a bare IP, so "the platform trust store said no" cannot be
//! the end of the story. It is, however, the *default*: a trust decision is
//! only ever offered to the user, never taken on their behalf.

/// Everything a trust prompt needs in order to be an informed decision.
///
/// Carries two distinct digests, and conflating them is the classic bug in
/// this area:
///
/// * [`Self::sha256_fingerprint`] is over the whole DER certificate. This is
///   what `openssl x509 -fingerprint -sha256` and every browser's certificate
///   viewer display, so it is the one a user can independently verify.
/// * [`Self::spki_sha256_base64`] is over the SubjectPublicKeyInfo. This is
///   what OkHttp's `CertificatePinner` consumes, so it is the one handed to
///   Media3 for its byte-range requests.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CertificateDetails {
    /// Colon-grouped uppercase hex over the whole DER certificate, e.g.
    /// `AB:CD:...`. Shown to the user; matches what other tools display.
    pub sha256_fingerprint: String,

    /// Base64 SHA-256 over the SubjectPublicKeyInfo, formatted for OkHttp's
    /// `CertificatePinner` as `sha256/<base64>`.
    pub spki_sha256_base64: String,

    /// The certificate's subject distinguished name.
    pub subject: String,

    /// The certificate's issuer distinguished name.
    pub issuer: String,

    /// Start of the validity window, as a Unix timestamp in seconds.
    pub not_before_unix: i64,

    /// End of the validity window, as a Unix timestamp in seconds.
    pub not_after_unix: i64,

    /// Every `subjectAltName` entry, dNSName and iPAddress alike. A pin is
    /// permission to trust *this certificate for this host* -- never a
    /// wildcard -- so the SAN list is checked on every subsequent handshake.
    pub subject_alt_names: Vec<String>,

    /// The certificate serial number, lowercase hex.
    pub serial_hex: String,

    /// Whether subject and issuer match, i.e. the certificate is self-signed.
    /// Presentational only; it never relaxes verification.
    pub is_self_signed: bool,

    /// Whether the validity window had already closed when this was built.
    pub is_expired: bool,
}

impl CertificateDetails {
    /// Whether this certificate's SANs cover `host`.
    ///
    /// Wildcards match exactly one label, per RFC 6125: `*.example.com`
    /// matches `a.example.com` but neither `example.com` nor `a.b.example.com`.
    #[must_use]
    pub fn covers_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.subject_alt_names
            .iter()
            .any(|san| san_matches(&san.trim_end_matches('.').to_ascii_lowercase(), &host))
    }
}

fn san_matches(san: &str, host: &str) -> bool {
    let Some(suffix) = san.strip_prefix("*.") else {
        return san == host;
    };
    // A wildcard covers exactly one label, and never the bare domain.
    match host.split_once('.') {
        Some((label, rest)) => !label.is_empty() && rest == suffix,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(sans: &[&str]) -> CertificateDetails {
        CertificateDetails {
            sha256_fingerprint: "AA:BB".to_owned(),
            spki_sha256_base64: "c3BraQ==".to_owned(),
            subject: "CN=beam.local".to_owned(),
            issuer: "CN=beam.local".to_owned(),
            not_before_unix: 0,
            not_after_unix: i64::MAX,
            subject_alt_names: sans.iter().map(|s| (*s).to_owned()).collect(),
            serial_hex: "01".to_owned(),
            is_self_signed: true,
            is_expired: false,
        }
    }

    #[test]
    fn an_exact_san_matches_its_host() {
        assert!(details(&["beam.local"]).covers_host("beam.local"));
        assert!(!details(&["beam.local"]).covers_host("other.local"));
    }

    #[test]
    fn matching_ignores_case_and_a_trailing_root_dot() {
        assert!(details(&["Beam.Local"]).covers_host("beam.local."));
    }

    #[test]
    fn a_wildcard_covers_exactly_one_label() {
        let cert = details(&["*.beam.local"]);
        assert!(cert.covers_host("media.beam.local"));
        // Neither the bare domain nor a deeper subdomain is covered.
        assert!(!cert.covers_host("beam.local"));
        assert!(!cert.covers_host("a.media.beam.local"));
    }

    #[test]
    fn a_bare_ip_san_matches_that_ip() {
        assert!(details(&["192.168.1.10"]).covers_host("192.168.1.10"));
        assert!(!details(&["192.168.1.10"]).covers_host("192.168.1.11"));
    }

    #[test]
    fn a_certificate_with_no_sans_covers_nothing() {
        // Notably it does not fall back to the subject CN: CN-as-hostname has
        // been deprecated since RFC 2818 and is not honoured here.
        assert!(!details(&[]).covers_host("beam.local"));
    }
}
