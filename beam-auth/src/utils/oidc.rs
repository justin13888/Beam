//! OIDC Authorization Code + PKCE, behind a trait so beam-server's login
//! flow never touches the `openidconnect` crate directly (see ADR-0003).
//!
//! [`DiscoveredOidcClient`] wraps a real IdP via discovery; [`FakeOidcClient`]
//! (test-utils only) is a programmable double for subcutaneous tests that
//! never touch the network.

use async_trait::async_trait;
use thiserror::Error;

/// The state a caller must persist (e.g. in `pending_auths`) between issuing
/// the login redirect and completing the callback.
#[derive(Debug, Clone)]
pub struct BeginAuth {
    /// The URL to redirect the browser to.
    pub auth_url: String,
    /// CSRF state value; the callback must present the same value.
    pub state: String,
    /// Nonce bound into the ID token at exchange time.
    pub nonce: String,
    /// PKCE code verifier, presented at exchange time.
    pub pkce_verifier: String,
}

/// The verified identity claims from a completed OIDC exchange.
#[derive(Debug, Clone)]
pub struct OidcIdentity {
    /// The `iss` claim -- half of the JIT-provisioning lookup key.
    pub issuer: String,
    /// The `sub` claim -- the other half.
    pub subject: String,
    pub email: Option<String>,
    /// Whether the IdP asserts the email is verified. Informational only --
    /// admin is derived from a configured claim, not the email (issue #85).
    pub email_verified: bool,
    pub name: Option<String>,
    pub picture: Option<String>,
    /// The full, already-verified ID-token claim set as raw JSON, so callers
    /// can evaluate a deployment-configured admin claim (see
    /// [`crate::utils::admin_claim`]) -- including non-standard claims like
    /// `groups` that the typed OIDC claim set discards. `Value::Null` when the
    /// claim set was unavailable or not an object.
    pub claims: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum OidcError {
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
    #[error("authorization code exchange failed: {0}")]
    Exchange(String),
    #[error("IdP response did not include an ID token")]
    MissingIdToken,
    #[error("ID token claims verification failed: {0}")]
    ClaimsVerification(String),
    #[error("nonce mismatch")]
    NonceMismatch,
}

/// The whole OIDC conversation, abstracted so the rest of the auth flow
/// never depends on a specific OIDC crate.
#[async_trait]
pub trait OidcClient: Send + Sync + std::fmt::Debug {
    /// Begins an Authorization Code + PKCE flow: mints state/nonce/PKCE and
    /// builds the redirect URL. The caller is responsible for persisting the
    /// returned `state`/`nonce`/`pkce_verifier` (e.g. in `pending_auths`)
    /// until the callback arrives. Errors if OIDC isn't configured/reachable
    /// -- there is no partial/degraded login to fall back to.
    fn begin_auth(&self) -> Result<BeginAuth, OidcError>;

    /// Exchanges an authorization code for tokens and verifies the ID
    /// token's claims (including that `nonce` matches what was minted by
    /// `begin_auth`).
    async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
        nonce: &str,
    ) -> Result<OidcIdentity, OidcError>;
}

#[cfg(feature = "oidc")]
mod discovered {
    use super::{BeginAuth, OidcClient, OidcError, OidcIdentity};
    use async_trait::async_trait;
    use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
    use openidconnect::{
        AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
        EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
        TokenResponse,
    };

    /// The exact endpoint typestate `CoreClient::from_provider_metadata(...)`
    /// produces: the authorization endpoint is always present after
    /// discovery (`EndpointSet`); device-auth/introspection/revocation are
    /// never populated from discovery (`EndpointNotSet`); token/userinfo are
    /// `EndpointMaybeSet` because OIDC discovery technically allows either to
    /// be absent (in practice a real IdP always sends both, but the type
    /// only promises "maybe").
    type DiscoveredCoreClient = CoreClient<
        EndpointSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointMaybeSet,
        EndpointMaybeSet,
    >;

    /// Real OIDC client, backed by discovery against a configured issuer.
    /// Construct once at process startup (discovery is an async network
    /// call); the resulting client is reused for every login.
    #[derive(Debug)]
    pub struct DiscoveredOidcClient {
        client: DiscoveredCoreClient,
        http_client: reqwest::Client,
        scopes: Vec<String>,
    }

    impl DiscoveredOidcClient {
        pub async fn discover(
            issuer: &str,
            client_id: &str,
            client_secret: &str,
            redirect_url: &str,
            scopes: Vec<String>,
        ) -> Result<Self, OidcError> {
            let http_client = reqwest::ClientBuilder::new()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| OidcError::Discovery(e.to_string()))?;

            let issuer_url = IssuerUrl::new(issuer.to_string())
                .map_err(|e| OidcError::Discovery(e.to_string()))?;
            let provider_metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client)
                .await
                .map_err(|e| OidcError::Discovery(e.to_string()))?;

            let redirect_url = RedirectUrl::new(redirect_url.to_string())
                .map_err(|e| OidcError::Discovery(e.to_string()))?;

            let client = CoreClient::from_provider_metadata(
                provider_metadata,
                ClientId::new(client_id.to_string()),
                Some(ClientSecret::new(client_secret.to_string())),
            )
            .set_redirect_uri(redirect_url);

            Ok(Self {
                client,
                http_client,
                scopes,
            })
        }
    }

    #[async_trait]
    impl OidcClient for DiscoveredOidcClient {
        fn begin_auth(&self) -> Result<BeginAuth, OidcError> {
            let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

            let mut auth_request = self.client.authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            );
            for scope in &self.scopes {
                auth_request = auth_request.add_scope(Scope::new(scope.clone()));
            }
            let (auth_url, csrf_token, nonce) =
                auth_request.set_pkce_challenge(pkce_challenge).url();

            Ok(BeginAuth {
                auth_url: auth_url.to_string(),
                state: csrf_token.secret().clone(),
                nonce: nonce.secret().clone(),
                pkce_verifier: pkce_verifier.secret().clone(),
            })
        }

        async fn exchange_code(
            &self,
            code: &str,
            pkce_verifier: &str,
            nonce: &str,
        ) -> Result<OidcIdentity, OidcError> {
            let token_response = self
                .client
                .exchange_code(AuthorizationCode::new(code.to_string()))
                .map_err(|e| OidcError::Exchange(e.to_string()))?
                .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_string()))
                .request_async(&self.http_client)
                .await
                .map_err(|e| OidcError::Exchange(e.to_string()))?;

            let id_token = token_response.id_token().ok_or(OidcError::MissingIdToken)?;

            let expected_nonce = Nonce::new(nonce.to_string());
            let claims = id_token
                .claims(&self.client.id_token_verifier(), &expected_nonce)
                .map_err(|e| OidcError::ClaimsVerification(e.to_string()))?;

            // `claims` above verified the token's signature and nonce, but the
            // typed `CoreIdTokenClaims` (with `EmptyAdditionalClaims`) drops any
            // non-standard claim -- e.g. the `groups`/`roles` a deployment binds
            // admin to (issue #85). Re-decode the now-trusted payload as raw JSON
            // to carry every claim through for admin evaluation.
            let raw_claims = decode_claims_payload(&id_token.to_string());

            Ok(OidcIdentity {
                issuer: claims.issuer().as_str().to_string(),
                subject: claims.subject().as_str().to_string(),
                email: claims.email().map(|e| e.as_str().to_string()),
                email_verified: claims.email_verified().unwrap_or(false),
                name: claims
                    .name()
                    .and_then(|n| n.get(None))
                    .map(|n| n.as_str().to_string()),
                picture: claims
                    .picture()
                    .and_then(|p| p.get(None))
                    .map(|p| p.as_str().to_string()),
                claims: raw_claims,
            })
        }
    }

    /// Decodes the claim-set (payload) segment of an already-verified compact
    /// JWT into raw JSON. The signature and nonce were validated by the caller
    /// before this runs, so the bytes are trusted; any decode failure yields
    /// `Value::Null` (admin is then simply never granted) rather than an error.
    ///
    /// Takes the compact string rather than a `CoreIdToken`, because that type
    /// can only be built by signing and verifying a real JWT -- which put the
    /// decoding, and the admin-claim evaluation that depends on it, out of
    /// reach of every test. The caller stringifies at the one call site.
    pub(crate) fn decode_claims_payload(compact: &str) -> serde_json::Value {
        use base64::Engine;

        let Some(payload_b64) = compact.split('.').nth(1) else {
            return serde_json::Value::Null;
        };
        let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)
        else {
            return serde_json::Value::Null;
        };
        serde_json::from_slice(&payload).unwrap_or(serde_json::Value::Null)
    }

    #[cfg(test)]
    mod claims_payload_tests {
        use super::decode_claims_payload;
        use base64::Engine as _;
        use serde_json::{Value, json};

        fn compact_jwt(payload: &Value) -> String {
            let encode =
                |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
            format!(
                "{}.{}.{}",
                encode(br#"{"alg":"RS256"}"#),
                encode(payload.to_string().as_bytes()),
                encode(b"not-checked-here")
            )
        }

        #[test]
        fn every_claim_in_the_payload_is_carried_through() {
            // This raw claim set is what admin evaluation reads; dropping it
            // silently means nobody is ever an admin, with no error anywhere.
            let payload = json!({
                "sub": "subj-1",
                "groups": ["beam-admin", "everyone"],
                "is_admin": true,
                "nested": { "a": 1 },
            });
            assert_eq!(decode_claims_payload(&compact_jwt(&payload)), payload);
        }

        #[test]
        fn the_header_and_signature_segments_are_ignored() {
            // Only the middle segment is the claim set; reading the first
            // would return the algorithm header instead.
            let decoded = decode_claims_payload(&compact_jwt(&json!({"sub": "subj-1"})));
            assert_eq!(decoded["sub"], "subj-1");
            assert!(decoded.get("alg").is_none());
        }

        #[test]
        fn a_malformed_token_decodes_to_null_rather_than_failing_the_login() {
            // A login that already passed signature and nonce verification must
            // not be rejected here; the worst case is no admin claim.
            for malformed in [
                "",
                "not-a-jwt",
                "onlyheader.",
                "header.!!!not-base64!!!.sig",
                &format!(
                    "header.{}.sig",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json")
                ),
            ] {
                assert_eq!(
                    decode_claims_payload(malformed),
                    Value::Null,
                    "for {malformed:?}"
                );
            }
        }
    }
}

#[cfg(feature = "oidc")]
pub use discovered::DiscoveredOidcClient;

/// A production-usable stand-in for when OIDC isn't configured (missing
/// issuer/client id/secret) or discovery failed at startup. Every call
/// returns a clear, descriptive error instead of panicking -- login is
/// simply unavailable until the deployment is configured correctly.
#[derive(Debug)]
pub struct NotConfiguredOidcClient {
    reason: String,
}

impl NotConfiguredOidcClient {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl OidcClient for NotConfiguredOidcClient {
    fn begin_auth(&self) -> Result<BeginAuth, OidcError> {
        Err(OidcError::Discovery(self.reason.clone()))
    }

    async fn exchange_code(
        &self,
        _code: &str,
        _pkce_verifier: &str,
        _nonce: &str,
    ) -> Result<OidcIdentity, OidcError> {
        Err(OidcError::Discovery(self.reason.clone()))
    }
}

/// Programmable [`OidcClient`] double for tests. Verifies the nonce/PKCE
/// verifier round-trip the same way a real IdP implicitly would (via ID
/// token claim verification / an authorization-server-side check), so tests
/// exercising a tampered or replayed callback see the same failure mode.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fake {
    use super::{BeginAuth, OidcClient, OidcError, OidcIdentity};
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct FakeOidcClient {
        last_begin: Mutex<Option<BeginAuth>>,
        response: Mutex<Result<OidcIdentity, String>>,
        begin_counter: Mutex<u64>,
    }

    impl Default for FakeOidcClient {
        fn default() -> Self {
            Self {
                last_begin: Mutex::new(None),
                response: Mutex::new(Err("no identity configured".to_string())),
                begin_counter: Mutex::new(0),
            }
        }
    }

    impl FakeOidcClient {
        /// Configures the identity `exchange_code` returns on a successful,
        /// well-formed exchange.
        pub fn with_identity(identity: OidcIdentity) -> Self {
            let client = Self::default();
            *client.response.lock().unwrap() = Ok(identity);
            client
        }

        /// Configures `exchange_code` to fail as if the IdP itself rejected
        /// the exchange (e.g. expired code, IdP outage).
        pub fn with_exchange_error(message: impl Into<String>) -> Self {
            let client = Self::default();
            *client.response.lock().unwrap() = Err(message.into());
            client
        }

        /// The most recent state/nonce/PKCE verifier minted by `begin_auth`,
        /// for tests that need to simulate a caller presenting the "right"
        /// values back (or deliberately tampered ones).
        pub fn last_begin(&self) -> Option<BeginAuth> {
            self.last_begin.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl OidcClient for FakeOidcClient {
        fn begin_auth(&self) -> Result<BeginAuth, OidcError> {
            let mut counter = self.begin_counter.lock().unwrap();
            *counter += 1;
            let begin = BeginAuth {
                auth_url: format!("https://fake-idp.test/authorize?n={counter}"),
                state: format!("fake-state-{counter}"),
                nonce: format!("fake-nonce-{counter}"),
                pkce_verifier: format!("fake-verifier-{counter}"),
            };
            *self.last_begin.lock().unwrap() = Some(begin.clone());
            Ok(begin)
        }

        async fn exchange_code(
            &self,
            _code: &str,
            pkce_verifier: &str,
            nonce: &str,
        ) -> Result<OidcIdentity, OidcError> {
            if let Some(begin) = self.last_begin.lock().unwrap().as_ref() {
                if begin.nonce != nonce {
                    return Err(OidcError::NonceMismatch);
                }
                if begin.pkce_verifier != pkce_verifier {
                    return Err(OidcError::Exchange(
                        "pkce verifier does not match".to_string(),
                    ));
                }
            }

            self.response
                .lock()
                .unwrap()
                .clone()
                .map_err(OidcError::Exchange)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use fake::FakeOidcClient;
