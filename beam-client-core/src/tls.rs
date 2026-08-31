//! TLS configuration, and the trust-on-first-use decision that self-hosted
//! servers make unavoidable.
//!
//! Two things are settled here that nothing else can settle:
//!
//! 1. **A crypto provider is installed.** `reqwest` is built with
//!    `rustls-tls-manual-roots-no-provider`, which supplies neither roots nor
//!    a provider. `ClientBuilder::build()` *panics* with "No provider set"
//!    when neither has been arranged -- it does not return `Err` -- so this
//!    is not a fallback path, it is the only path.
//! 2. **A rejected certificate is remembered.** rustls reports a verification
//!    failure as an opaque `Error`, and by the time it surfaces at the call
//!    site the certificate is gone. Without capturing it here the trust
//!    prompt would have to open a second connection to the server that just
//!    failed, and race it. [`TrustDecision`] holds what was rejected so
//!    `map_error` can turn a transport failure into
//!    [`BeamError::UntrustedCertificate`] with the details already in hand.
//!
//! The pin is over the **whole DER certificate**, not the SubjectPublicKeyInfo.
//! That is deliberate: it is the digest `openssl x509 -fingerprint -sha256`
//! and every browser certificate viewer displays, so a user comparing what the
//! app shows against what their server reports is comparing like with like.
//! `spki_sha256_base64` is carried alongside for OkHttp's `CertificatePinner`,
//! which speaks only SPKI.

use crate::error::BeamError;
use crate::trust::CertificateDetails;
use base64::Engine as _;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, RwLock};

/// Install the process-wide rustls crypto provider, once.
///
/// `ring` rather than `aws-lc-rs`: aws-lc-rs needs cmake and a per-ABI C
/// toolchain, which does not cross-compile cleanly to Android.
///
/// Installing twice is not an error worth propagating -- another crate in the
/// same process may legitimately have installed the same provider first -- so
/// a second call is a no-op.
pub fn install_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // Ignores the result: `install_default` fails only when a provider is
        // already installed, which is the outcome this function wants anyway.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// What one server's verifier rejected, and what the user has agreed to.
///
/// Shared between the verifier and the client that owns it, so trusting a
/// certificate takes effect on the next handshake without rebuilding the
/// `reqwest::Client` -- which would discard the connection pool that Media3's
/// neighbouring range requests depend on.
#[derive(Debug, Default)]
pub struct TrustDecision {
    trusted: RwLock<Vec<String>>,
    rejected: Mutex<Option<(String, CertificateDetails)>>,
}

impl TrustDecision {
    /// A decision seeded with the fingerprints the user already accepted.
    #[must_use]
    pub fn new(trusted: Vec<String>) -> Self {
        Self {
            trusted: RwLock::new(trusted.iter().map(|value| normalize(value)).collect()),
            rejected: Mutex::new(None),
        }
    }

    /// Accept a fingerprint from now on.
    pub fn trust(&self, fingerprint: &str) {
        let normalized = normalize(fingerprint);
        let mut trusted = self.trusted.write().expect("trust lock");
        if !trusted.contains(&normalized) {
            trusted.push(normalized);
        }
    }

    /// Whether this fingerprint has been accepted.
    #[must_use]
    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trusted
            .read()
            .expect("trust lock")
            .contains(&normalize(fingerprint))
    }

    /// Take the certificate most recently rejected, clearing it.
    ///
    /// Taking rather than reading: a stale rejection reported against a later,
    /// unrelated network failure would show the user a trust prompt for a
    /// problem that no longer exists.
    #[must_use]
    pub fn take_rejection(&self) -> Option<(String, CertificateDetails)> {
        self.rejected.lock().expect("rejection lock").take()
    }

    fn record_rejection(&self, host: String, details: CertificateDetails) {
        *self.rejected.lock().expect("rejection lock") = Some((host, details));
    }
}

/// Compare fingerprints without being defeated by formatting.
///
/// The same digest reaches this code as `AB:CD:EF`, `ab:cd:ef` and `abcdef`
/// depending on whether it came from the UI, storage, or a paste from
/// `openssl`. Comparing the raw strings would silently reject a certificate
/// the user had already trusted.
fn normalize(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_lowercase)
        .collect()
}

/// The platform verifier, with a user-approved exception.
#[derive(Debug)]
struct TofuVerifier {
    inner: Arc<WebPkiServerVerifier>,
    decision: Arc<TrustDecision>,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // The public trust store is always consulted first. A certificate that
        // verifies normally never reaches the pinning path, so a pin can only
        // ever widen trust for one specific certificate -- it can never be the
        // reason a publicly-valid certificate is rejected.
        let public = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        let Err(public_error) = public else {
            return Ok(ServerCertVerified::assertion());
        };

        let host = match server_name {
            ServerName::DnsName(name) => name.as_ref().to_owned(),
            ServerName::IpAddress(address) => format!("{address:?}"),
            _ => String::new(),
        };

        let Some(details) = describe(end_entity) else {
            // Unparseable certificates are rejected outright rather than
            // offered to the user: there is nothing truthful to show them.
            return Err(public_error);
        };

        // Three conditions, all required. Dropping the SAN check would turn a
        // pin into a wildcard for every host the device later connects to,
        // and dropping the expiry check would make a pin permanent.
        let pinned = self.decision.is_trusted(&details.sha256_fingerprint);
        let covers = details.covers_host(&host);
        // The whole validity window, not just the far end. A certificate whose
        // window has not opened yet is as invalid as an expired one, and
        // checking only expiry would accept one a public CA would refuse.
        let seconds = now.as_secs();
        let live = seconds >= details.not_before_unix.max(0).unsigned_abs()
            && seconds <= details.not_after_unix.max(0).unsigned_abs();

        if pinned && covers && live {
            return Ok(ServerCertVerified::assertion());
        }

        self.decision.record_rejection(host, details);
        Err(public_error)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A rustls configuration that honours public roots plus this server's pins.
///
/// # Errors
///
/// Returns [`BeamError::Network`] when the verifier cannot be constructed,
/// which in practice means the bundled root store is empty.
pub fn client_config(decision: Arc<TrustDecision>) -> Result<rustls::ClientConfig, BeamError> {
    install_crypto_provider();

    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = rustls::crypto::ring::default_provider();
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(provider))
        .build()
        .map_err(|error| BeamError::Network {
            detail: format!("could not build the certificate verifier: {error}"),
            retryable: false,
        })?;

    Ok(rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TofuVerifier { inner, decision }))
        .with_no_client_auth())
}

/// Everything a trust prompt needs, read out of a DER certificate.
///
/// Returns `None` when the bytes do not parse as X.509 at all.
#[must_use]
pub fn describe(der: &CertificateDer<'_>) -> Option<CertificateDetails> {
    use x509_parser::prelude::*;

    let (_, certificate) = X509Certificate::from_der(der.as_ref()).ok()?;

    let sha256_fingerprint = {
        let digest = Sha256::digest(der.as_ref());
        digest
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    };

    let spki_sha256_base64 = base64::engine::general_purpose::STANDARD
        .encode(Sha256::digest(certificate.tbs_certificate.subject_pki.raw));

    let subject_alt_names = certificate
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|extension| {
            extension
                .value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(value) => Some((*value).to_owned()),
                    GeneralName::IPAddress(bytes) => render_ip(bytes),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let subject = certificate.subject().to_string();
    let issuer = certificate.issuer().to_string();
    let not_before_unix = certificate.validity().not_before.timestamp();
    let not_after_unix = certificate.validity().not_after.timestamp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed());

    Some(CertificateDetails {
        sha256_fingerprint,
        spki_sha256_base64,
        is_self_signed: subject == issuer,
        subject,
        issuer,
        not_before_unix,
        not_after_unix,
        subject_alt_names,
        serial_hex: certificate.raw_serial_as_string().replace(':', ""),
        is_expired: now > not_after_unix,
    })
}

/// Render an `iPAddress` SAN, which arrives as raw network-order bytes.
fn render_ip(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => {
            let octets: [u8; 4] = bytes.try_into().ok()?;
            Some(std::net::Ipv4Addr::from(octets).to_string())
        }
        16 => {
            let octets: [u8; 16] = bytes.try_into().ok()?;
            Some(std::net::Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::CertificateDer;

    /// A real self-signed certificate for the given SANs.
    fn certificate(sans: &[&str]) -> CertificateDer<'static> {
        let names: Vec<String> = sans.iter().map(|name| (*name).to_owned()).collect();
        let certified = rcgen::generate_simple_self_signed(names).expect("generate");
        certified.cert.der().clone()
    }

    #[test]
    fn a_certificate_is_described_from_its_der() {
        let der = certificate(&["beam.local", "192.168.1.10"]);
        let details = describe(&der).expect("a generated certificate parses");

        assert!(details.is_self_signed);
        assert!(!details.is_expired);
        assert!(details.covers_host("beam.local"));
        assert!(details.covers_host("192.168.1.10"));
        assert!(!details.covers_host("elsewhere.local"));
        // 32 bytes rendered as colon-separated hex pairs.
        assert_eq!(details.sha256_fingerprint.len(), 32 * 3 - 1);
        assert!(!details.spki_sha256_base64.is_empty());
    }

    #[test]
    fn the_fingerprint_is_over_the_whole_certificate() {
        // The digest must match what `openssl x509 -fingerprint -sha256`
        // prints, because that is what a user is asked to compare against.
        let der = certificate(&["beam.local"]);
        let details = describe(&der).expect("parses");

        let expected = Sha256::digest(der.as_ref())
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        assert_eq!(details.sha256_fingerprint, expected);
    }

    #[test]
    fn two_certificates_do_not_share_a_fingerprint() {
        let first = describe(&certificate(&["beam.local"])).expect("parses");
        let second = describe(&certificate(&["beam.local"])).expect("parses");
        assert_ne!(first.sha256_fingerprint, second.sha256_fingerprint);
    }

    #[test]
    fn bytes_that_are_not_a_certificate_describe_as_nothing() {
        let garbage = CertificateDer::from(vec![0x00, 0x01, 0x02, 0x03]);
        assert!(describe(&garbage).is_none());
    }

    #[test]
    fn a_fingerprint_is_matched_regardless_of_formatting() {
        let decision = TrustDecision::new(vec!["AB:CD:EF".to_owned()]);
        assert!(decision.is_trusted("ab:cd:ef"));
        assert!(decision.is_trusted("abcdef"));
        assert!(decision.is_trusted("AB CD EF"));
        assert!(!decision.is_trusted("AB:CD:E0"));
    }

    #[test]
    fn trusting_a_fingerprint_takes_effect_without_a_rebuild() {
        let decision = TrustDecision::new(Vec::new());
        assert!(!decision.is_trusted("AA:BB"));

        decision.trust("aa:bb");
        assert!(decision.is_trusted("AA:BB"));

        // Trusting twice must not accumulate duplicates.
        decision.trust("AA:BB");
        assert_eq!(decision.trusted.read().expect("lock").len(), 1);
    }

    #[test]
    fn a_rejection_is_taken_exactly_once() {
        let decision = TrustDecision::new(Vec::new());
        assert!(decision.take_rejection().is_none());

        let details = describe(&certificate(&["beam.local"])).expect("parses");
        decision.record_rejection("beam.local".to_owned(), details);

        assert!(decision.take_rejection().is_some());
        assert!(
            decision.take_rejection().is_none(),
            "a stale rejection would prompt about a problem that no longer exists"
        );
    }

    #[test]
    fn an_untrusted_self_signed_certificate_is_rejected_and_recorded() {
        install_crypto_provider();
        let decision = Arc::new(TrustDecision::new(Vec::new()));
        let verifier = verifier(Arc::clone(&decision));
        let der = certificate(&["beam.local"]);

        let outcome = verifier.verify_server_cert(
            &der,
            &[],
            &ServerName::try_from("beam.local").expect("name"),
            &[],
            UnixTime::now(),
        );

        assert!(outcome.is_err(), "an unpinned self-signed cert must fail");
        let (host, details) = decision.take_rejection().expect("the prompt needs details");
        assert_eq!(host, "beam.local");
        assert!(details.is_self_signed);
    }

    #[test]
    fn a_pinned_certificate_is_accepted_for_the_host_it_covers() {
        install_crypto_provider();
        let der = certificate(&["beam.local"]);
        let details = describe(&der).expect("parses");
        let decision = Arc::new(TrustDecision::new(vec![details.sha256_fingerprint]));
        let verifier = verifier(Arc::clone(&decision));

        let outcome = verifier.verify_server_cert(
            &der,
            &[],
            &ServerName::try_from("beam.local").expect("name"),
            &[],
            UnixTime::now(),
        );

        assert!(outcome.is_ok(), "the user pinned this exact certificate");
        assert!(decision.take_rejection().is_none());
    }

    #[test]
    fn a_pin_does_not_generalise_to_another_host() {
        // The bug this guards against: treating a pin as "trust this
        // certificate everywhere" rather than "trust it for this server". A
        // certificate pinned for the home server would otherwise be accepted
        // for any host that could present it.
        install_crypto_provider();
        let der = certificate(&["beam.local"]);
        let details = describe(&der).expect("parses");
        let decision = Arc::new(TrustDecision::new(vec![details.sha256_fingerprint]));
        let verifier = verifier(Arc::clone(&decision));

        let outcome = verifier.verify_server_cert(
            &der,
            &[],
            &ServerName::try_from("bank.example.com").expect("name"),
            &[],
            UnixTime::now(),
        );

        assert!(outcome.is_err(), "a pin is per-host, never a wildcard");
    }

    #[test]
    fn an_unrelated_certificate_is_rejected_even_when_another_is_pinned() {
        install_crypto_provider();
        let pinned = describe(&certificate(&["beam.local"])).expect("parses");
        let decision = Arc::new(TrustDecision::new(vec![pinned.sha256_fingerprint]));
        let verifier = verifier(Arc::clone(&decision));

        // A different certificate for the same host -- the substitution a
        // pin exists to detect.
        let impostor = certificate(&["beam.local"]);
        let outcome = verifier.verify_server_cert(
            &impostor,
            &[],
            &ServerName::try_from("beam.local").expect("name"),
            &[],
            UnixTime::now(),
        );

        assert!(outcome.is_err(), "only the pinned certificate is accepted");
    }

    fn verifier(decision: Arc<TrustDecision>) -> TofuVerifier {
        install_crypto_provider();
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = rustls::crypto::ring::default_provider();
        let inner =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(provider))
                .build()
                .expect("verifier builds");
        TofuVerifier { inner, decision }
    }

    #[test]
    fn a_pinned_certificate_is_rejected_outside_its_validity_window() {
        // Both ends of the window. Checking only expiry would accept a
        // certificate a public CA would refuse for not having started yet.
        install_crypto_provider();
        let der = certificate(&["beam.local"]);
        let details = describe(&der).expect("parses");
        let decision = Arc::new(TrustDecision::new(vec![details.sha256_fingerprint.clone()]));
        let verifier = verifier(Arc::clone(&decision));
        let name = ServerName::try_from("beam.local").expect("name");

        let before_it_starts = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            details
                .not_before_unix
                .max(0)
                .unsigned_abs()
                .saturating_sub(3_600),
        ));
        assert!(
            verifier
                .verify_server_cert(&der, &[], &name, &[], before_it_starts)
                .is_err(),
            "a certificate whose validity has not begun must be refused"
        );

        let after_it_ends = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            details.not_after_unix.max(0).unsigned_abs() + 3_600,
        ));
        assert!(
            verifier
                .verify_server_cert(&der, &[], &name, &[], after_it_ends)
                .is_err(),
            "an expired certificate must be refused even when pinned"
        );
    }

    #[test]
    fn a_client_config_can_be_built() {
        // Guards the panic this module exists to prevent: without an installed
        // provider, building a client aborts the process.
        let config = client_config(Arc::new(TrustDecision::new(Vec::new())));
        assert!(config.is_ok());
        assert!(
            reqwest::Client::builder()
                .use_preconfigured_tls(config.expect("config"))
                .build()
                .is_ok()
        );
    }
}
