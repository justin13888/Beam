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
mod discovered {
    use super::{BeginAuth, DeviceAuthStart, DevicePoll, OidcClient, OidcError, OidcIdentity};
    use async_trait::async_trait;
    use openidconnect::core::{
        CoreAuthDisplay, CoreAuthenticationFlow, CoreClaimName, CoreClaimType, CoreClient,
        CoreClientAuthMethod, CoreDeviceAuthorizationResponse, CoreGrantType, CoreIdToken,
        CoreIdTokenClaims, CoreJsonWebKey, CoreJweContentEncryptionAlgorithm,
        CoreJweKeyManagementAlgorithm, CoreResponseMode, CoreResponseType,
        CoreSubjectIdentifierType, CoreTokenResponse,
    };
    use openidconnect::{
        AdditionalProviderMetadata, AsyncHttpClient, HttpClientError, HttpRequest, HttpResponse,
        ProviderMetadata,
    };
    use openidconnect::{
        AuthorizationCode, ClientId, ClientSecret, CsrfToken, DeviceAuthorizationUrl,
        DeviceCodeErrorResponse, DeviceCodeErrorResponseType, EndpointMaybeSet, EndpointNotSet,
        EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
        TokenResponse,
    };
    use serde::{Deserialize, Serialize};
    use std::future::Future;
    use std::pin::Pin;
    use std::time::Duration;

    /// The HTTP client `openidconnect` makes discovery, JWKS, and token
    /// requests through.
    ///
    /// oauth2 5.0.0 implements [`AsyncHttpClient`] only for reqwest 0.12; this
    /// is the same adapter (ported from its `reqwest_client.rs`) over the
    /// workspace's reqwest 0.13, so the server links one reqwest (issue #132).
    #[derive(Debug, Clone)]
    pub(crate) struct OidcHttpClient(reqwest::Client);

    /// How long establishing a connection to the IdP may take.
    ///
    /// Fixed rather than configurable: discovery runs once at startup and the
    /// exchange runs inside a user's login request, and neither has a caller
    /// that would pick a different bound. Without one, an IdP that accepts a
    /// connection and never answers stalls startup, or the login, forever.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    /// How long a whole request -- connect, send, and reading the full
    /// response -- may take. Covers the IdP that connects but never responds.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    impl OidcHttpClient {
        pub(crate) fn new() -> Result<Self, reqwest::Error> {
            Self::with_timeouts(CONNECT_TIMEOUT, REQUEST_TIMEOUT)
        }

        /// [`Self::new`] with explicit bounds, so a test can prove the bound
        /// is enforced without waiting out the production one.
        fn with_timeouts(connect: Duration, request: Duration) -> Result<Self, reqwest::Error> {
            // Redirects are never followed: an IdP endpoint that answers with
            // a redirect is surfaced as that response, not chased to wherever
            // it points (the SSRF guidance of the OIDC/OAuth 2.0 specs).
            let client = reqwest::ClientBuilder::new()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(connect)
                .timeout(request)
                .build()?;
            Ok(Self(client))
        }
    }

    impl<'c> AsyncHttpClient<'c> for OidcHttpClient {
        type Error = HttpClientError<reqwest::Error>;
        type Future =
            Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + Sync + 'c>>;

        fn call(&'c self, request: HttpRequest) -> Self::Future {
            Box::pin(async move {
                let request = reqwest::Request::try_from(request).map_err(Box::new)?;
                let response = self.0.execute(request).await.map_err(Box::new)?;
                into_http_response(response).await
            })
        }
    }

    /// Copies a reqwest response into the `http::Response` `openidconnect`
    /// parses: status, version, every header (repeated ones included), and
    /// the full body.
    async fn into_http_response(
        response: reqwest::Response,
    ) -> Result<HttpResponse, HttpClientError<reqwest::Error>> {
        let mut builder = openidconnect::http::Response::builder()
            .status(response.status())
            .version(response.version());
        for (name, value) in response.headers() {
            builder = builder.header(name, value);
        }
        let body = response.bytes().await.map_err(Box::new)?;
        builder.body(body.to_vec()).map_err(HttpClientError::Http)
    }

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

    /// The one discovery field `CoreProviderMetadata` drops that Beam reads:
    /// RFC 8628's `device_authorization_endpoint` (RFC 8414 section 2
    /// registers it in the discovery document). Optional, because plenty of
    /// IdPs do not offer the grant -- its absence turns the device login off,
    /// not discovery.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub(crate) struct DeviceAwareMetadata {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_authorization_endpoint: Option<DeviceAuthorizationUrl>,
    }

    impl AdditionalProviderMetadata for DeviceAwareMetadata {}

    /// `CoreProviderMetadata` with [`DeviceAwareMetadata`] in place of
    /// `EmptyAdditionalProviderMetadata` -- the pattern openidconnect's own
    /// `okta_device_grant` example uses.
    pub(crate) type DeviceAwareProviderMetadata = ProviderMetadata<
        DeviceAwareMetadata,
        CoreAuthDisplay,
        CoreClientAuthMethod,
        CoreClaimName,
        CoreClaimType,
        CoreGrantType,
        CoreJweContentEncryptionAlgorithm,
        CoreJweKeyManagementAlgorithm,
        CoreJsonWebKey,
        CoreResponseMode,
        CoreResponseType,
        CoreSubjectIdentifierType,
    >;

    /// The device authorization endpoint a discovery document names, if any.
    pub(crate) fn device_endpoint(
        metadata: &DeviceAwareProviderMetadata,
    ) -> Option<DeviceAuthorizationUrl> {
        metadata
            .additional_metadata()
            .device_authorization_endpoint
            .clone()
    }

    /// How Beam authenticates its device-grant requests to the IdP.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum DeviceClientAuth {
        /// `client_secret_post`: `client_id` and `client_secret` in the form.
        Post,
        /// `client_secret_basic`: the credentials in an `Authorization`
        /// header, `client_id` repeated in the form as an identifier.
        Basic,
    }

    /// Picks the device-grant client authentication from discovery.
    ///
    /// `client_secret_post` whenever the IdP advertises it. The device
    /// authorization request carries `client_id` in its form regardless (RFC
    /// 8628 section 3.1), and Dex -- found driving v2.45.1 -- reads the client
    /// secret only from that form: given Basic credentials it records an
    /// empty secret with the device request, then fails its own internal code
    /// exchange at approval with `invalid_client`. Otherwise
    /// `client_secret_basic`, which RFC 8414 section 2 makes the default when
    /// the document lists nothing.
    pub(crate) fn device_client_auth(metadata: &DeviceAwareProviderMetadata) -> DeviceClientAuth {
        let advertises_post = metadata
            .token_endpoint_auth_methods_supported()
            .is_some_and(|methods| methods.contains(&CoreClientAuthMethod::ClientSecretPost));
        if advertises_post {
            DeviceClientAuth::Post
        } else {
            DeviceClientAuth::Basic
        }
    }

    /// RFC 8628 section 3.4's grant type for polling the token endpoint.
    const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

    /// Real OIDC client, backed by discovery against a configured issuer.
    /// Construct once at process startup (discovery is an async network
    /// call); the resulting client is reused for every login.
    #[derive(Debug)]
    pub struct DiscoveredOidcClient {
        client: DiscoveredCoreClient,
        http_client: OidcHttpClient,
        scopes: Vec<String>,
        /// From discovery; `None` when the IdP does not offer the grant.
        device_url: Option<DeviceAuthorizationUrl>,
        /// From discovery: how the device-grant requests authenticate.
        device_auth: DeviceClientAuth,
        /// Kept for the device-token poll, which Beam sends itself (see
        /// `poll_device_token`).
        client_id: String,
        client_secret: String,
    }

    impl DiscoveredOidcClient {
        pub async fn discover(
            issuer: &str,
            client_id: &str,
            client_secret: &str,
            redirect_url: &str,
            scopes: Vec<String>,
        ) -> Result<Self, OidcError> {
            let http_client =
                OidcHttpClient::new().map_err(|e| OidcError::Discovery(e.to_string()))?;

            let issuer_url = IssuerUrl::new(issuer.to_string())
                .map_err(|e| OidcError::Discovery(e.to_string()))?;
            let provider_metadata =
                DeviceAwareProviderMetadata::discover_async(issuer_url, &http_client)
                    .await
                    .map_err(|e| OidcError::Discovery(e.to_string()))?;
            let device_url = device_endpoint(&provider_metadata);
            let device_auth = device_client_auth(&provider_metadata);

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
                device_url,
                device_auth,
                client_id: client_id.to_owned(),
                client_secret: client_secret.to_owned(),
            })
        }

        /// The one form POST RFC 8628 section 3.4 describes, authenticated
        /// the way the device authorization request was.
        fn device_token_request(
            &self,
            token_url: &str,
            device_code: &str,
        ) -> Result<HttpRequest, OidcError> {
            device_token_request(
                token_url,
                &self.client_id,
                &self.client_secret,
                device_code,
                self.device_auth,
            )
        }
    }

    /// Builds the device-code poll. Free-standing so a test can read exactly
    /// what goes on the wire without an IdP.
    pub(crate) fn device_token_request(
        token_url: &str,
        client_id: &str,
        client_secret: &str,
        device_code: &str,
        auth: DeviceClientAuth,
    ) -> Result<HttpRequest, OidcError> {
        use base64::Engine as _;
        use openidconnect::http::{Method, header};
        use openidconnect::url::form_urlencoded;

        let encode =
            |value: &str| -> String { form_urlencoded::byte_serialize(value.as_bytes()).collect() };
        let credential = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            encode(client_id),
            encode(client_secret)
        ));
        let mut form = form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", DEVICE_CODE_GRANT)
            .append_pair("device_code", device_code)
            // An identifier some IdPs read only from the form, whichever way
            // the client authenticates.
            .append_pair("client_id", client_id);
        if auth == DeviceClientAuth::Post {
            form.append_pair("client_secret", client_secret);
        }
        let body = form.finish();

        let mut builder = openidconnect::http::Request::builder()
            .method(Method::POST)
            .uri(token_url)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        // One authentication method per request (RFC 6749 section 2.3).
        if auth == DeviceClientAuth::Basic {
            builder = builder.header(header::AUTHORIZATION, format!("Basic {credential}"));
        }
        builder
            .body(body.into_bytes())
            .map_err(|e| OidcError::Exchange(e.to_string()))
    }

    /// What one device-token poll answered, before any ID token is verified.
    #[derive(Debug)]
    pub(crate) enum DeviceTokenOutcome {
        Pending,
        SlowDown,
        Denied,
        Expired,
        Issued(Box<CoreTokenResponse>),
    }

    /// Classifies a token-endpoint response to a device-code poll (RFC 8628
    /// section 3.5). Pure, so every branch is reachable from a table.
    ///
    /// A success status is a token response. Anything else is read as an
    /// OAuth error body: the four RFC 8628 codes map to their outcomes, and
    /// `invalid_grant` -- what an IdP answers for a device code it no longer
    /// holds -- is read as expiry, since the user's remedy is the same:
    /// start again. Every other error is an exchange failure.
    pub(crate) fn classify_device_token_response(
        status: openidconnect::http::StatusCode,
        body: &[u8],
    ) -> Result<DeviceTokenOutcome, OidcError> {
        if status.is_success() {
            let token: CoreTokenResponse = serde_json::from_slice(body).map_err(|e| {
                OidcError::Exchange(format!("unreadable device token response: {e}"))
            })?;
            return Ok(DeviceTokenOutcome::Issued(Box::new(token)));
        }

        let error: DeviceCodeErrorResponse = serde_json::from_slice(body).map_err(|e| {
            OidcError::Exchange(format!(
                "device token request failed with {status} and an unreadable body: {e}"
            ))
        })?;
        match error.error() {
            DeviceCodeErrorResponseType::AuthorizationPending => Ok(DeviceTokenOutcome::Pending),
            DeviceCodeErrorResponseType::SlowDown => Ok(DeviceTokenOutcome::SlowDown),
            DeviceCodeErrorResponseType::AccessDenied => Ok(DeviceTokenOutcome::Denied),
            DeviceCodeErrorResponseType::ExpiredToken => Ok(DeviceTokenOutcome::Expired),
            DeviceCodeErrorResponseType::Basic(
                openidconnect::core::CoreErrorResponseType::InvalidGrant,
            ) => Ok(DeviceTokenOutcome::Expired),
            other => Err(OidcError::Exchange(format!(
                "device token request failed with {status}: {other}"
            ))),
        }
    }

    /// The identity a verified ID token asserts.
    ///
    /// Shared by the code exchange and the device grant so the two logins
    /// cannot disagree about who somebody is.
    fn identity_from(id_token: &CoreIdToken, claims: &CoreIdTokenClaims) -> OidcIdentity {
        // `claims` verified the token's signature (and, for the code flow,
        // its nonce), but the typed `CoreIdTokenClaims` (with
        // `EmptyAdditionalClaims`) drops any non-standard claim -- e.g. the
        // `groups`/`roles` a deployment binds admin to (issue #85). Re-decode
        // the now-trusted payload as raw JSON to carry every claim through for
        // admin evaluation.
        let raw_claims = decode_claims_payload(&id_token.to_string());

        OidcIdentity {
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

            Ok(identity_from(id_token, claims))
        }

        async fn begin_device_auth(&self) -> Result<DeviceAuthStart, OidcError> {
            let device_url = self
                .device_url
                .clone()
                .ok_or(OidcError::DeviceFlowUnsupported)?;
            let client = self.client.clone().set_device_authorization_url(device_url);

            // `openid` is skipped below because openidconnect already adds it.
            let client = match self.device_auth {
                // oauth2 puts `client_id` and `client_secret` in the form.
                DeviceClientAuth::Post => {
                    client.set_auth_type(openidconnect::AuthType::RequestBody)
                }
                DeviceClientAuth::Basic => client,
            };
            let mut request = client.exchange_device_code();
            if self.device_auth == DeviceClientAuth::Basic {
                // `client_id` in the form as well as in the Basic header: RFC
                // 8628 section 3.1 makes it optional for an authenticated
                // client, but some IdPs read it from the form only. It is an
                // identifier, not a second authentication method.
                request = request.add_extra_param("client_id", self.client_id.clone());
            }
            for scope in self
                .scopes
                .iter()
                .filter(|scope| scope.as_str() != "openid")
            {
                request = request.add_scope(Scope::new(scope.clone()));
            }
            let details: CoreDeviceAuthorizationResponse = request
                .request_async(&self.http_client)
                .await
                .map_err(|e| OidcError::Exchange(e.to_string()))?;

            Ok(DeviceAuthStart {
                device_code: details.device_code().secret().clone(),
                user_code: details.user_code().secret().clone(),
                verification_uri: details.verification_uri().to_string(),
                verification_uri_complete: details
                    .verification_uri_complete()
                    .map(|uri| uri.secret().clone()),
                expires_in_secs: details.expires_in().as_secs(),
                interval_secs: details.interval().as_secs(),
            })
        }

        /// One request, sent by Beam rather than by `oauth2`: oauth2 5.0.0's
        /// `DeviceAccessTokenRequest` polls in a loop until the user decides,
        /// and the single-request step inside it is private. Beam's client
        /// does the pacing (ADR-0017), so a handler makes exactly one request
        /// and returns.
        async fn poll_device_token(&self, device_code: &str) -> Result<DevicePoll, OidcError> {
            // An IdP that stopped offering the grant since the flow began is
            // not polled with a code it may no longer honour.
            if self.device_url.is_none() {
                return Err(OidcError::DeviceFlowUnsupported);
            }
            let token_url = self
                .client
                .token_uri()
                .ok_or_else(|| {
                    OidcError::Exchange("the identity provider has no token endpoint".to_owned())
                })?
                .to_string();

            let request = self.device_token_request(&token_url, device_code)?;
            let response = self
                .http_client
                .call(request)
                .await
                .map_err(|e| OidcError::Exchange(e.to_string()))?;

            let token = match classify_device_token_response(response.status(), response.body())? {
                DeviceTokenOutcome::Pending => return Ok(DevicePoll::Pending),
                DeviceTokenOutcome::SlowDown => return Ok(DevicePoll::SlowDown),
                DeviceTokenOutcome::Denied => return Ok(DevicePoll::Denied),
                DeviceTokenOutcome::Expired => return Ok(DevicePoll::Expired),
                DeviceTokenOutcome::Issued(token) => token,
            };

            let id_token = token.id_token().ok_or(OidcError::MissingIdToken)?;
            // No nonce: the device grant has no authorization request to bind
            // one into (RFC 8628 section 3.1 takes only `client_id` and
            // `scope`). Signature, issuer, audience and expiry are all still
            // verified.
            let no_nonce = |_: Option<&Nonce>| -> Result<(), String> { Ok(()) };
            let claims = id_token
                .claims(&self.client.id_token_verifier(), no_nonce)
                .map_err(|e| OidcError::ClaimsVerification(e.to_string()))?;

            Ok(DevicePoll::Complete(identity_from(id_token, claims)))
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
    mod http_client_tests {
        use super::{OidcHttpClient, into_http_response};
        use openidconnect::http::{self, Method, StatusCode, Version, header::LOCATION};
        use openidconnect::{AsyncHttpClient, HttpClientError};
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        /// Reads one HTTP/1.1 request (head plus a `Content-Length` body)
        /// off `stream` and returns it raw.
        async fn read_request(stream: &mut TcpStream) -> String {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed mid-request");
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw);
                let Some(head_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text[..head_end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if raw.len() >= head_end + 4 + content_length {
                    return String::from_utf8(raw).unwrap();
                }
            }
        }

        #[tokio::test]
        async fn a_redirect_is_returned_as_is_and_its_target_is_never_contacted() {
            // Following an IdP's redirect would let whoever controls that
            // response aim the server at an arbitrary host (SSRF), so the 3xx
            // must come back to `openidconnect` unfollowed.
            let idp = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let elsewhere_url = format!("http://{}/", elsewhere.local_addr().unwrap());
            let idp_url = format!("http://{}/token", idp.local_addr().unwrap());

            let location = elsewhere_url.clone();
            let server = tokio::spawn(async move {
                let (mut stream, _) = idp.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                request
            });

            let body = b"grant_type=authorization_code&code=abc".to_vec();
            let request = http::Request::builder()
                .method(Method::POST)
                .uri(&idp_url)
                .header("x-beam-probe", "forwarded")
                .body(body.clone())
                .unwrap();

            let client = OidcHttpClient::new().unwrap();
            let response = client.call(request).await.unwrap();

            assert_eq!(response.status(), StatusCode::FOUND);
            assert_eq!(response.headers()[LOCATION], elsewhere_url.as_str());

            // The request reached the IdP as `openidconnect` built it.
            let received = server.await.unwrap();
            assert!(
                received.starts_with("POST /token HTTP/1.1\r\n"),
                "{received}"
            );
            assert!(
                received
                    .to_ascii_lowercase()
                    .contains("\r\nx-beam-probe: forwarded\r\n"),
                "{received}"
            );
            assert!(
                received.ends_with(std::str::from_utf8(&body).unwrap()),
                "{received}"
            );

            // A followed redirect would have left a connection queued here.
            let elsewhere = elsewhere.into_std().unwrap();
            elsewhere.set_nonblocking(true).unwrap();
            assert_eq!(
                elsewhere.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "the redirect target was contacted"
            );
        }

        #[tokio::test]
        async fn an_unreachable_idp_is_a_transport_error() {
            // Bind then drop, so the port is known to have no listener.
            let addr = TcpListener::bind("127.0.0.1:0")
                .await
                .unwrap()
                .local_addr()
                .unwrap();
            let request = http::Request::builder()
                .uri(format!("http://{addr}/.well-known/openid-configuration"))
                .body(Vec::new())
                .unwrap();

            let result = OidcHttpClient::new().unwrap().call(request).await;

            match result {
                Err(HttpClientError::Reqwest(error)) => assert!(error.is_connect(), "{error}"),
                other => panic!("expected a reqwest connect error, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn an_idp_that_never_answers_times_out() {
            // An IdP that accepts the connection and then goes silent must not
            // hold discovery (startup) or a code exchange (a login) forever.
            let idp = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", idp.local_addr().unwrap());
            let _server = tokio::spawn(async move {
                let (mut stream, _) = idp.accept().await.unwrap();
                // Read until the client gives up and closes; never reply.
                let mut sink = Vec::new();
                let _ = stream.read_to_end(&mut sink).await;
            });
            let request = http::Request::builder().uri(url).body(Vec::new()).unwrap();
            let client = OidcHttpClient::with_timeouts(
                Duration::from_millis(200),
                Duration::from_millis(200),
            )
            .unwrap();

            // The outer bound only turns a missing timeout into a failure
            // instead of a hung test run.
            let result = tokio::time::timeout(Duration::from_secs(10), client.call(request))
                .await
                .expect("the request was not bounded by the client's timeout");

            match result {
                Err(HttpClientError::Reqwest(error)) => assert!(error.is_timeout(), "{error}"),
                other => panic!("expected a reqwest timeout error, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_response_is_copied_with_status_version_every_header_and_body() {
            // `openidconnect` parses this copy, so a dropped header, a
            // collapsed repeat, or a lost body misreports why discovery or the
            // exchange failed.
            let upstream = http::Response::builder()
                .status(StatusCode::FOUND)
                .version(Version::HTTP_2)
                .header(LOCATION, "https://elsewhere.test/")
                .header("x-repeated", "first")
                .header("x-repeated", "second")
                .body("redirect body")
                .unwrap();

            let converted = into_http_response(reqwest::Response::from(upstream))
                .await
                .unwrap();

            assert_eq!(converted.status(), StatusCode::FOUND);
            assert_eq!(converted.version(), Version::HTTP_2);
            assert_eq!(converted.headers()[LOCATION], "https://elsewhere.test/");
            let repeated: Vec<_> = converted.headers().get_all("x-repeated").iter().collect();
            assert_eq!(repeated, ["first", "second"]);
            assert_eq!(converted.body().as_slice(), b"redirect body");
        }
    }

    #[cfg(test)]
    mod device_grant_tests {
        use super::{
            DeviceAwareProviderMetadata, DeviceClientAuth, DeviceTokenOutcome,
            classify_device_token_response, device_client_auth, device_endpoint,
            device_token_request,
        };
        use crate::utils::oidc::OidcError;
        use base64::Engine as _;
        use openidconnect::TokenResponse as _;
        use openidconnect::http::{Method, StatusCode, header};
        use serde_json::{Value, json};

        fn discovery(extra: Value) -> DeviceAwareProviderMetadata {
            let mut document = json!({
                "issuer": "https://idp.test",
                "authorization_endpoint": "https://idp.test/auth",
                "token_endpoint": "https://idp.test/token",
                "jwks_uri": "https://idp.test/keys",
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
            });
            document
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(document).expect("a valid discovery document")
        }

        #[test]
        fn a_discovery_document_naming_a_device_endpoint_enables_the_grant() {
            let metadata = discovery(json!({
                "device_authorization_endpoint": "https://idp.test/device/code",
            }));
            assert_eq!(
                device_endpoint(&metadata).map(|url| url.to_string()),
                Some("https://idp.test/device/code".to_owned())
            );
        }

        #[test]
        fn a_discovery_document_without_one_still_discovers_with_the_grant_off() {
            // Most IdPs that do not offer the grant simply omit the field; that
            // must not fail discovery and take the browser login down with it.
            assert!(device_endpoint(&discovery(json!({}))).is_none());
        }

        #[test]
        fn device_requests_post_the_secret_when_the_idp_advertises_it() {
            // Dex advertises both and needs the form: with Basic it approves
            // the user and then fails its own exchange with invalid_client.
            let both = discovery(json!({
                "token_endpoint_auth_methods_supported":
                    ["client_secret_basic", "client_secret_post"],
            }));
            assert_eq!(device_client_auth(&both), DeviceClientAuth::Post);

            let basic_only = discovery(json!({
                "token_endpoint_auth_methods_supported": ["client_secret_basic"],
            }));
            assert_eq!(device_client_auth(&basic_only), DeviceClientAuth::Basic);

            // RFC 8414: an absent list means client_secret_basic.
            assert_eq!(
                device_client_auth(&discovery(json!({}))),
                DeviceClientAuth::Basic
            );
        }

        fn body(value: Value) -> Vec<u8> {
            value.to_string().into_bytes()
        }

        #[test]
        fn each_rfc_8628_error_code_maps_to_its_outcome() {
            for (code, expected) in [
                ("authorization_pending", "Pending"),
                ("slow_down", "SlowDown"),
                ("access_denied", "Denied"),
                ("expired_token", "Expired"),
                // An IdP that no longer holds the device code: start again.
                ("invalid_grant", "Expired"),
            ] {
                let outcome = classify_device_token_response(
                    StatusCode::BAD_REQUEST,
                    &body(json!({ "error": code })),
                )
                .unwrap_or_else(|e| panic!("{code} must classify, got {e}"));
                let name = match outcome {
                    DeviceTokenOutcome::Pending => "Pending",
                    DeviceTokenOutcome::SlowDown => "SlowDown",
                    DeviceTokenOutcome::Denied => "Denied",
                    DeviceTokenOutcome::Expired => "Expired",
                    DeviceTokenOutcome::Issued(_) => "Issued",
                };
                assert_eq!(name, expected, "for {code}");
            }
        }

        #[test]
        fn any_other_oauth_error_is_an_exchange_failure() {
            // `invalid_client` is a misconfigured deployment, not a user who
            // has not decided yet: polling on would never end.
            for (status, code) in [
                (StatusCode::UNAUTHORIZED, "invalid_client"),
                (StatusCode::BAD_REQUEST, "unsupported_grant_type"),
                (StatusCode::BAD_REQUEST, "server_error"),
            ] {
                let outcome =
                    classify_device_token_response(status, &body(json!({ "error": code })));
                assert!(
                    matches!(outcome, Err(OidcError::Exchange(ref message)) if message.contains(code)),
                    "{code} -> {outcome:?}"
                );
            }
        }

        #[test]
        fn an_error_status_with_an_unreadable_body_is_an_exchange_failure() {
            let outcome =
                classify_device_token_response(StatusCode::BAD_GATEWAY, b"<html>upstream</html>");
            assert!(
                matches!(outcome, Err(OidcError::Exchange(ref message)) if message.contains("502")),
                "{outcome:?}"
            );
        }

        fn compact_jwt(payload: &Value) -> String {
            let encode =
                |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
            format!(
                "{}.{}.{}",
                encode(br#"{"alg":"RS256","typ":"JWT"}"#),
                encode(payload.to_string().as_bytes()),
                encode(b"signature-checked-later")
            )
        }

        #[test]
        fn a_success_is_a_token_response_carrying_the_id_token() {
            let id_token = compact_jwt(&json!({
                "iss": "https://idp.test",
                "sub": "subj-1",
                "aud": "beam",
                "exp": 2_000_000_000u64,
                "iat": 1_700_000_000u64,
            }));
            let outcome = classify_device_token_response(
                StatusCode::OK,
                &body(json!({
                    "access_token": "access",
                    "token_type": "Bearer",
                    "id_token": id_token,
                })),
            )
            .unwrap();
            match outcome {
                DeviceTokenOutcome::Issued(token) => assert_eq!(
                    token.id_token().map(ToString::to_string),
                    Some(id_token),
                    "the ID token must reach verification byte-for-byte"
                ),
                _ => panic!("a 200 is an issued token"),
            }
        }

        #[test]
        fn a_success_that_is_not_a_token_response_is_an_exchange_failure() {
            let outcome = classify_device_token_response(StatusCode::OK, b"{\"hello\":1}");
            assert!(
                matches!(outcome, Err(OidcError::Exchange(_))),
                "{outcome:?}"
            );
        }

        fn poll_form(auth: DeviceClientAuth) -> (Vec<(String, String)>, Option<String>) {
            let request = device_token_request(
                "https://idp.test/token",
                "beam",
                "s3cret:with&odd=chars",
                "the-device-code",
                auth,
            )
            .unwrap();

            assert_eq!(request.method(), Method::POST);
            assert_eq!(request.uri(), "https://idp.test/token");
            assert_eq!(
                request.headers()[header::CONTENT_TYPE],
                "application/x-www-form-urlencoded"
            );
            let form = openidconnect::url::form_urlencoded::parse(request.body())
                .into_owned()
                .collect();
            let authorization = request
                .headers()
                .get(header::AUTHORIZATION)
                .map(|value| value.to_str().unwrap().to_owned());
            (form, authorization)
        }

        fn pair(name: &str, value: &str) -> (String, String) {
            (name.to_owned(), value.to_owned())
        }

        #[test]
        fn a_basic_poll_keeps_the_secret_in_the_header() {
            let (form, authorization) = poll_form(DeviceClientAuth::Basic);
            assert_eq!(
                form,
                [
                    pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    pair("device_code", "the-device-code"),
                    pair("client_id", "beam"),
                ],
                "with client_secret_basic the secret never enters the body"
            );

            // RFC 6749 section 2.3.1: id and secret are each form-encoded
            // before being joined and base64-encoded.
            let authorization = authorization.expect("basic auth");
            let encoded = authorization.strip_prefix("Basic ").expect("basic auth");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            assert_eq!(
                String::from_utf8(decoded).unwrap(),
                "beam:s3cret%3Awith%26odd%3Dchars"
            );
        }

        #[test]
        fn a_post_poll_carries_the_secret_in_the_form_and_no_header() {
            let (form, authorization) = poll_form(DeviceClientAuth::Post);
            assert_eq!(
                form,
                [
                    pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    pair("device_code", "the-device-code"),
                    pair("client_id", "beam"),
                    pair("client_secret", "s3cret:with&odd=chars"),
                ]
            );
            assert_eq!(
                authorization, None,
                "one authentication method per request (RFC 6749 section 2.3)"
            );
        }
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
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fake {
    use super::{BeginAuth, DeviceAuthStart, DevicePoll, OidcClient, OidcError, OidcIdentity};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct FakeOidcClient {
        last_begin: Mutex<Option<BeginAuth>>,
        response: Mutex<Result<OidcIdentity, String>>,
        begin_counter: Mutex<u64>,
        /// `false` models an IdP whose discovery names no device endpoint.
        device_flow: bool,
        /// Answers for successive device polls, front first. Once empty,
        /// every poll is `Pending` -- the user simply has not decided.
        device_script: Mutex<VecDeque<Result<DevicePoll, String>>>,
        device_polls: Mutex<u64>,
        device_begins: Mutex<u64>,
        /// Every device code the server polled with, in order.
        polled_codes: Mutex<Vec<String>>,
    }

    impl Default for FakeOidcClient {
        fn default() -> Self {
            Self {
                last_begin: Mutex::new(None),
                response: Mutex::new(Err("no identity configured".to_string())),
                begin_counter: Mutex::new(0),
                device_flow: true,
                device_script: Mutex::new(VecDeque::new()),
                device_polls: Mutex::new(0),
                device_begins: Mutex::new(0),
                polled_codes: Mutex::new(Vec::new()),
            }
        }
    }

    /// The user code every fake device authorization shows.
    pub const FAKE_USER_CODE: &str = "BCDF-GHJK";
    /// The interval every fake device authorization asks for.
    pub const FAKE_DEVICE_INTERVAL_SECS: u64 = 5;
    /// The lifetime every fake device authorization grants.
    pub const FAKE_DEVICE_EXPIRES_IN_SECS: u64 = 600;

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

        /// Scripts what successive device-token polls answer. An `Err` is an
        /// IdP failure (`OidcError::Exchange`). Polls past the end of the
        /// script are `Pending`.
        #[must_use]
        pub fn with_device_script(self, script: Vec<Result<DevicePoll, String>>) -> Self {
            *self.device_script.lock().unwrap() = script.into();
            self
        }

        /// An IdP that does not offer the device authorization grant.
        #[must_use]
        pub fn without_device_flow(mut self) -> Self {
            self.device_flow = false;
            self
        }

        /// How many times the token endpoint was polled with a device code.
        pub fn device_poll_count(&self) -> u64 {
            *self.device_polls.lock().unwrap()
        }

        /// Every device code the token endpoint was polled with, in order.
        pub fn polled_device_codes(&self) -> Vec<String> {
            self.polled_codes.lock().unwrap().clone()
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

        async fn begin_device_auth(&self) -> Result<DeviceAuthStart, OidcError> {
            if !self.device_flow {
                return Err(OidcError::DeviceFlowUnsupported);
            }
            let mut begins = self.device_begins.lock().unwrap();
            *begins += 1;
            Ok(DeviceAuthStart {
                device_code: format!("fake-device-code-{begins}"),
                user_code: FAKE_USER_CODE.to_owned(),
                verification_uri: "https://fake-idp.test/device".to_owned(),
                verification_uri_complete: Some(format!(
                    "https://fake-idp.test/device?user_code={FAKE_USER_CODE}"
                )),
                expires_in_secs: FAKE_DEVICE_EXPIRES_IN_SECS,
                interval_secs: FAKE_DEVICE_INTERVAL_SECS,
            })
        }

        async fn poll_device_token(&self, device_code: &str) -> Result<DevicePoll, OidcError> {
            if !self.device_flow {
                return Err(OidcError::DeviceFlowUnsupported);
            }
            *self.device_polls.lock().unwrap() += 1;
            self.polled_codes
                .lock()
                .unwrap()
                .push(device_code.to_owned());
            self.device_script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(DevicePoll::Pending))
                .map_err(OidcError::Exchange)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use fake::FakeOidcClient;
