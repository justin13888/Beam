//! OIDC Authorization Code + PKCE, and the RFC 8628 device authorization
//! grant, behind a trait so beam-server's login flows never touch the
//! `openidconnect` crate directly (see ADR-0003 and ADR-0017).
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

/// What the IdP's device authorization endpoint answered (RFC 8628 §3.2):
/// the secret `device_code` the server polls with, and what the user is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthStart {
    /// The secret the token endpoint is polled with. Never leaves the server.
    pub device_code: String,
    /// The short code the user types at `verification_uri`.
    pub user_code: String,
    /// Where the user goes, on any device with a browser, to approve.
    pub verification_uri: String,
    /// `verification_uri` with the user code already filled in, when the IdP
    /// offers one (e.g. for a QR code).
    pub verification_uri_complete: Option<String>,
    /// How long the device code stays valid, in seconds.
    pub expires_in_secs: u64,
    /// The minimum wait between polls the IdP asks for, in seconds.
    pub interval_secs: u64,
}

/// The outcome of one poll of the IdP's token endpoint with a device code
/// (RFC 8628 §3.5).
#[derive(Debug, Clone)]
pub enum DevicePoll {
    /// The user has not finished approving yet.
    Pending,
    /// The user has not finished, and the poll came too fast: the interval
    /// must grow by five seconds.
    SlowDown,
    /// The user refused.
    Denied,
    /// The device code expired, or the IdP no longer recognises it.
    Expired,
    /// The user approved; the ID token verified.
    Complete(OidcIdentity),
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
    /// The IdP's discovery document names no `device_authorization_endpoint`,
    /// so the device grant cannot be offered (the browser flow still works).
    #[error("the identity provider does not support the device authorization grant")]
    DeviceFlowUnsupported,
}

/// How Beam's OIDC client is registered to authenticate at the IdP's token
/// and device authorization endpoints (`BEAM_OIDC_CLIENT_AUTH_METHOD`).
///
/// The names are RFC 7591's `token_endpoint_auth_method` values, the ones an
/// IdP's client registration uses. One method covers every request Beam sends
/// with the client secret -- the authorization-code exchange and both
/// device-grant requests -- because an IdP pins it per client and accepts
/// nothing else from that client.
///
/// An operator setting rather than something Beam works out (ADR-0017
/// D151-9): discovery's `token_endpoint_auth_methods_supported` lists what the
/// IdP can accept from *some* client, not what *this* client is registered
/// with, and that is published nowhere. Nor can a refusal be relied on to
/// reveal it: Dex's device endpoint accepts a Basic-authenticated request,
/// records an empty secret, and fails only when the user approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuthMethod {
    /// `client_secret_basic`: the credentials in an HTTP Basic
    /// `Authorization` header (RFC 6749 section 2.3.1), the method RFC 7591
    /// and RFC 8414 make the default. The device-grant requests repeat
    /// `client_id` in the form as an identifier, since some IdPs read it only
    /// from there.
    ClientSecretBasic,
    /// `client_secret_post`: `client_id` and `client_secret` in the form
    /// body, and no `Authorization` header.
    ClientSecretPost,
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

    /// Begins an RFC 8628 device authorization grant: asks the IdP's device
    /// authorization endpoint for a device code and the user code to show.
    /// [`OidcError::DeviceFlowUnsupported`] when the IdP has no such endpoint.
    async fn begin_device_auth(&self) -> Result<DeviceAuthStart, OidcError>;

    /// Polls the IdP's token endpoint **once** with `device_code`.
    ///
    /// One request per call, never a loop: the caller -- a client's own poll
    /// -- decides the pacing, so a request handler never sleeps waiting for a
    /// user who may never approve.
    async fn poll_device_token(&self, device_code: &str) -> Result<DevicePoll, OidcError>;
}

#[cfg(feature = "oidc")]
mod discovered;

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

    async fn begin_device_auth(&self) -> Result<DeviceAuthStart, OidcError> {
        Err(OidcError::Discovery(self.reason.clone()))
    }

    async fn poll_device_token(&self, _device_code: &str) -> Result<DevicePoll, OidcError> {
        Err(OidcError::Discovery(self.reason.clone()))
    }
}

/// Programmable [`OidcClient`] double for tests. Verifies the nonce/PKCE
/// verifier round-trip the same way a real IdP implicitly would (via ID
/// token claim verification / an authorization-server-side check), so tests
/// exercising a tampered or replayed callback see the same failure mode.
///
/// Not marked `#[mutants::skip]`: an attribute proc macro on a non-inline
/// module is unstable, so `.cargo/mutants.toml` excludes the file by path.
#[cfg(any(test, feature = "test-utils"))]
pub mod fake;

#[cfg(any(test, feature = "test-utils"))]
pub use fake::FakeOidcClient;
