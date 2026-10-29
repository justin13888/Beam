//! The surface Kotlin and Swift actually call.
//!
//! Everything here is a thin shell over the modules beside it. The logic lives
//! in those modules, where it is testable as plain Rust; this file only
//! marshals. Keeping the split strict is what stops UniFFI's generated
//! scaffolding from dominating the crate's coverage, and it means a Kotlin
//! integration test and a Rust unit test are exercising the same code.

mod admin;
mod catalog;
mod playback;
mod progress;
mod telemetry;

pub use playback::{PlaybackHttpConfig, PlaybackPlan, RebufferMonitor, ServerHttpConfig};

use crate::api::Client as GeneratedClient;
use crate::api::{MiddlewareBackend, ReqwestBackend};
use crate::capability::DeviceProfile;
use crate::catalog::{DeviceSession, ServerHealth};
use crate::clock::{Clock, SystemClock};
use crate::error::BeamError;
use crate::ports::kv::KeyValueStore;
use crate::progress::{ProgressQueue, ProgressThrottle};
use crate::servers::{ServerRecord, normalize_base_url, server_id_for};
use crate::session::{
    DeviceLoginPrompt, DeviceLoginStep, SessionEffect, SessionEvent, SessionState, UserSummary,
};
use crate::telemetry::{TelemetryClientKind, TelemetryQueue};
use crate::transport::{ABOUT_BLANK, FailureKind, SessionMiddleware, TransportFailure, classify};
use secrecy::{ExposeSecret, SecretString};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Storage key holding the list of known server ids.
const SERVER_INDEX_KEY: &str = "servers/index";

/// Storage key holding the currently selected server id.
const ACTIVE_SERVER_KEY: &str = "servers/active";

/// A server as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerSummary {
    /// Stable identifier.
    pub id: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Origin, absolute.
    pub base_url: String,
    /// Authentication state for this server.
    pub state: SessionState,
    /// Whether this is the server operations default to.
    pub is_active: bool,
}

/// The one live copy of a server's session cookie.
///
/// The generated client reads it through a [`crate::api::Credential::Provider`]
/// registered once, at install, so a sign-in or a sign-out is a write here
/// rather than a rebuilt client. The same handle is what
/// [`BeamClient::server_http_config`] reads to hand the credential to the
/// platform player.
#[derive(Default)]
struct SessionCookie(RwLock<Option<SecretString>>);

impl SessionCookie {
    /// A holder carrying `cookie`, which may be nothing.
    fn new(cookie: Option<SecretString>) -> Self {
        Self(RwLock::new(cookie))
    }

    /// The cookie to send, or none.
    fn get(&self) -> Option<SecretString> {
        self.0.read().expect("cookie lock").clone()
    }

    /// Make `cookie` the one sent from now on.
    fn set(&self, cookie: Option<SecretString>) {
        *self.0.write().expect("cookie lock") = cookie;
    }

    /// Whether a cookie is registered at all.
    fn is_registered(&self) -> bool {
        self.0.read().expect("cookie lock").is_some()
    }
}

impl std::fmt::Debug for SessionCookie {
    /// Hand-written, because the value here *is* the credential. `SecretString`
    /// already redacts itself and `Credential`'s own `Debug` prints
    /// `Provider(***)` without touching the closure that captures this, so
    /// nothing today would print it -- which is exactly why the guarantee is
    /// stated here rather than left resting on two other crates' choices.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = if self.is_registered() {
            "registered"
        } else {
            "none"
        };
        formatter.debug_tuple("SessionCookie").field(&held).finish()
    }
}

/// The secret-storage key a server's session cookie lives under.
fn session_key(server_id: &str) -> String {
    format!("session/{server_id}")
}

/// The secret-storage key holding who the server said a stored cookie
/// belongs to.
///
/// Secret rather than plain storage because it carries the user's email.
/// Written beside the cookie once the server confirms it, and deleted with
/// it, so a cold start can restore a signed-in session -- admin role
/// included -- without asking `/v1/me` first.
fn identity_key(server_id: &str) -> String {
    format!("identity/{server_id}")
}

/// [`UserSummary`] as it is stored, decoupled from the FFI record so that a
/// change to what the foreign side sees is not silently a change to what is
/// on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct StoredIdentity {
    id: String,
    display_name: String,
    email: Option<String>,
    is_admin: bool,
    avatar_url: Option<String>,
}

impl From<&UserSummary> for StoredIdentity {
    fn from(user: &UserSummary) -> Self {
        let UserSummary {
            id,
            display_name,
            email,
            is_admin,
            avatar_url,
        } = user;
        Self {
            id: id.clone(),
            display_name: display_name.clone(),
            email: email.clone(),
            is_admin: *is_admin,
            avatar_url: avatar_url.clone(),
        }
    }
}

impl From<StoredIdentity> for UserSummary {
    fn from(stored: StoredIdentity) -> Self {
        let StoredIdentity {
            id,
            display_name,
            email,
            is_admin,
            avatar_url,
        } = stored;
        Self {
            id,
            display_name,
            email,
            is_admin,
            avatar_url,
        }
    }
}

/// `user` as [`identity_key`] stores it.
fn encode_identity(user: &UserSummary) -> Result<String, BeamError> {
    serde_json::to_string(&StoredIdentity::from(user)).map_err(|error| BeamError::Storage {
        detail: error.to_string(),
    })
}

/// One server's live state.
struct ServerContext {
    record: ServerRecord,
    /// The generated client, built once with the `BeamSession` credential
    /// already registered. Never replaced on a session change: see
    /// [`ServerContext::set_session`].
    client: GeneratedClient,
    /// The session cookie the client sends, shared with the credential
    /// provider registered on it.
    cookie: Arc<SessionCookie>,
    middleware: Arc<SessionMiddleware>,
    state: SessionState,
    throttle: Arc<ProgressThrottle>,
    queue: Arc<ProgressQueue>,
    /// Playback telemetry waiting to be reported to this server. Per server,
    /// because every event names one of this server's files.
    telemetry: Arc<TelemetryQueue>,
    /// The certificates the user has accepted for this server, and the last
    /// one the verifier turned away.
    trust: Arc<crate::tls::TrustDecision>,
}

impl ServerContext {
    /// Make `cookie` the session this server's requests carry -- or none.
    ///
    /// The spec declares the `BeamSession` scheme on every operation that
    /// needs a session, and the generated client attaches the credential
    /// registered under that name as `Cookie: beam_session=<value>`. That
    /// credential is a [`crate::api::Credential::Provider`] reading this
    /// holder, registered once at install, so the cookie still has exactly one
    /// owner and changing it touches neither the client nor the transport.
    fn set_session(&self, cookie: Option<SecretString>) {
        self.cookie.set(cookie);
        // A 401 belongs to the era of the cookie that provoked it. Left
        // standing across a session boundary it would be consumed by some
        // unrelated later failure, which would then withdraw and delete a
        // credential the server had never rejected.
        self.middleware.clear_unauthorized();
    }
}

/// The `components.securitySchemes` key the spec declares for the session
/// cookie; the name a credential is registered under.
const SESSION_SCHEME: &str = "BeamSession";

/// What the credential provider says when the server has no session.
///
/// Reaches the façade only as the type of `Error::RequestConstruction`'s
/// source -- see the `RequestConstruction` arm of
/// [`crate::transport::TransportFailure::of`] -- so this text is for a log and
/// never for a decision. It names no cookie, because there is none.
const NO_SESSION: &str = "no BeamSession cookie is registered for this server";

/// The `BeamSession` credential: whatever cookie `holder` carries when a
/// request is built.
///
/// A provider rather than a fixed [`crate::api::Credential::ApiKey`], which is
/// the whole reason the client survives a session change. The generated client
/// awaits this while building a secured request -- before `build()` and before
/// the request reaches the transport -- so a server with no session still
/// refuses the call rather than sending it uncredentialled.
fn session_credential(holder: &Arc<SessionCookie>) -> crate::api::Credential {
    let holder = Arc::clone(holder);
    crate::api::Credential::Provider(Arc::new(move || {
        let holder = Arc::clone(&holder);
        Box::pin(async move {
            holder
                .get()
                .ok_or_else(|| crate::api::AuthError::new(NO_SESSION))
        })
    }))
}

impl std::fmt::Debug for ServerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerContext")
            .field("record", &self.record)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// The core, as the foreign side holds it.
///
/// # Request failures
///
/// Every operation that sends a request can fail the way any request can.
/// Those failures are listed once, here, and each operation's own `# Errors`
/// names only what it adds:
///
/// - [`BeamError::Network`] when no response arrived;
/// - [`BeamError::UntrustedCertificate`] when the handshake met a certificate
///   the user has not accepted;
/// - [`BeamError::Protocol`] when the response did not match the contract the
///   client was generated from;
/// - [`BeamError::Unauthenticated`] when there is no session, and
///   [`BeamError::SessionExpired`] when the server rejected the one there was;
/// - whatever the server answered: [`BeamError::BadRequest`],
///   [`BeamError::Forbidden`], [`BeamError::NotFound`],
///   [`BeamError::RateLimited`] or [`BeamError::Server`].
#[derive(Debug, uniffi::Object)]
pub struct BeamClient {
    storage: Arc<dyn KeyValueStore>,
    clock: Arc<dyn Clock>,
    servers: RwLock<HashMap<String, ServerContext>>,
    active: RwLock<Option<String>>,
    device_profile: RwLock<Option<DeviceProfile>>,
    /// Which kind of client playback telemetry reports as.
    client_kind: RwLock<TelemetryClientKind>,
    /// Servers whose rejected cookie is still in the platform keystore.
    ///
    /// `ClearCookie` on the 401 path is swallowed rather than returned -- the
    /// caller's error is the expired session, which stands, and the in-memory
    /// credential is already gone. Swallowing it alone was not safe: the
    /// cookie the server had just rejected was still in the keystore, and the
    /// next `restore` would register it and send it again. Naming the server
    /// here is what stops that. Held in memory, so it lasts as long as the
    /// process that failed the delete.
    abandoned_sessions: RwLock<HashSet<String>>,
}

/// A path parameter the API declares as `format: uuid`.
///
/// The FFI surface takes identifiers as strings, because that is what the
/// foreign bindings carry; the generated client takes a [`uuid::Uuid`], so a
/// malformed identifier is rejected here rather than on the wire.
fn parse_uuid(field: &str, value: &str) -> Result<uuid::Uuid, BeamError> {
    uuid::Uuid::parse_str(value).map_err(|_| BeamError::BadRequest {
        detail: format!("{field} is not a valid identifier"),
        code: ABOUT_BLANK.to_owned(),
    })
}

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// Build a core over the platform's storage.
    #[uniffi::constructor]
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>) -> Arc<Self> {
        Self::over(storage, Arc::new(SystemClock))
    }

    /// Load the server registry and any stored sessions.
    ///
    /// Sessions are restored optimistically: a stored cookie comes back
    /// [`SessionState::Authenticated`] as the identity the server confirmed
    /// when it was stored, and the first request the server refuses with a
    /// `401` expires it. That keeps app start off the network.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the registry or a stored session
    /// cannot be read, [`BeamError::InvalidServerUrl`] for a stored origin the
    /// HTTP client refuses, and [`BeamError::Network`], not retryable, when a
    /// server's HTTP client cannot be built.
    pub async fn restore(&self) -> Result<Vec<ServerSummary>, BeamError> {
        let ids: Vec<String> = match self.storage.get(SERVER_INDEX_KEY.to_owned()).await? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => Vec::new(),
        };

        for id in ids {
            let Some(raw) = self.storage.get(format!("servers/{id}")).await? else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<ServerRecord>(&raw) else {
                continue;
            };
            let session = if self.is_abandoned(&id) {
                // A cookie the device could not delete is not a session: the
                // server rejected it, and registering it again would put it
                // straight back on the wire. The delete is retried first,
                // because the keystore that refused it may since have
                // unlocked; the cookie is dropped either way.
                if self.storage.remove_secret(session_key(&id)).await.is_ok() {
                    self.forget_abandoned(&id);
                }
                let _ = self.storage.remove_secret(identity_key(&id)).await;
                None
            } else {
                self.stored_session(&id).await?
            };
            // A stored session is signed in as the identity the server
            // confirmed when it was stored: no request is made, and the first
            // `401` on the cookie expires it like any other session.
            let (cookie, state) = match session {
                Some((cookie, user)) => {
                    let (state, _) = crate::session::transition(
                        &SessionState::LoggedOut,
                        SessionEvent::StoredSessionRestored(Box::new(user)),
                    );
                    (Some(cookie), state)
                }
                None => (None, SessionState::LoggedOut),
            };
            self.install(record, cookie, state)?;
        }

        *self.active.write().expect("active lock") =
            self.storage.get(ACTIVE_SERVER_KEY.to_owned()).await?;
        self.list_servers()
    }

    /// Add a server, or return the existing one if it is already known.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::InvalidServerUrl`] for an unusable address,
    /// [`BeamError::Storage`] when the registry cannot be written, and
    /// [`BeamError::Network`], not retryable, when the server's HTTP client
    /// cannot be built.
    pub async fn add_server(
        &self,
        base_url: String,
        display_name: Option<String>,
    ) -> Result<ServerSummary, BeamError> {
        let origin = normalize_base_url(&base_url)?;
        let id = server_id_for(&origin);

        // Adding a server already present is idempotent rather than an error:
        // the user typing the same address twice means "go there", not "fail".
        if !self.servers.read().expect("servers lock").contains_key(&id) {
            let record = ServerRecord::new(&origin, display_name.as_deref(), self.clock.now_unix());
            // Installed before persisting, because `persist_record` writes the
            // index from the in-memory map. Persisting first wrote an index
            // that did not yet contain this server, so the very first server
            // added was never in it -- and `restore` found nothing, sending
            // the viewer back to the address field on every cold start.
            self.install(record.clone(), None, SessionState::LoggedOut)?;
            self.persist_record(&record).await?;
        }

        self.select_server(id.clone()).await?;
        self.summary(&id)
    }

    /// Every known server, by display name.
    ///
    /// # Errors
    ///
    /// Never returns one: the registry is read from memory, which
    /// [`Self::restore`] filled.
    pub fn list_servers(&self) -> Result<Vec<ServerSummary>, BeamError> {
        let servers = self.servers.read().expect("servers lock");
        let active = self.active.read().expect("active lock").clone();
        let mut summaries: Vec<ServerSummary> = servers
            .values()
            .map(|context| ServerSummary {
                id: context.record.id.clone(),
                display_name: context.record.display_name.clone(),
                base_url: context.record.base_url.clone(),
                state: context.state.clone(),
                is_active: active.as_deref() == Some(context.record.id.as_str()),
            })
            .collect();
        summaries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        Ok(summaries)
    }

    /// Make a server the default for later operations.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] if it is not registered, and
    /// [`BeamError::Storage`] when the choice cannot be persisted.
    pub async fn select_server(&self, server_id: String) -> Result<(), BeamError> {
        if !self
            .servers
            .read()
            .expect("servers lock")
            .contains_key(&server_id)
        {
            return Err(BeamError::UnknownServer { server_id });
        }
        self.storage
            .put(ACTIVE_SERVER_KEY.to_owned(), server_id.clone())
            .await?;
        *self.active.write().expect("active lock") = Some(server_id);
        Ok(())
    }

    /// Forget a server and everything tied to it.
    ///
    /// Clears the record, the session cookie, and the progress queue together.
    /// Leaving any one behind would be a leak across servers.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the removal cannot be persisted.
    pub async fn remove_server(&self, server_id: String) -> Result<(), BeamError> {
        self.servers
            .write()
            .expect("servers lock")
            .remove(&server_id);
        self.storage.remove(format!("servers/{server_id}")).await?;
        self.storage.remove_secret(session_key(&server_id)).await?;
        self.storage.remove_secret(identity_key(&server_id)).await?;
        self.forget_abandoned(&server_id);
        self.storage
            .remove(format!("progress_queue/{server_id}"))
            .await?;

        // The guard is taken and released before the await: holding a
        // std::sync guard across a suspension point makes the whole future
        // non-Send, which UniFFI's tokio runtime requires.
        let was_active = {
            let mut active = self.active.write().expect("active lock");
            if active.as_deref() == Some(server_id.as_str()) {
                *active = None;
                true
            } else {
                false
            }
        };
        if was_active {
            self.storage.remove(ACTIVE_SERVER_KEY.to_owned()).await?;
        }
        self.persist_index().await
    }

    /// The URL to open in the in-app browser to sign in.
    ///
    /// The server cannot redirect to a custom scheme -- `sanitize_redirect_path`
    /// accepts only same-origin relative paths -- so the foreign side watches
    /// for the `beam_session` cookie appearing instead of for a callback URL.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] if the server is not registered,
    /// and [`BeamError::InvalidServerUrl`] when its stored origin cannot be
    /// joined.
    pub fn login_url(&self, server_id: String) -> Result<String, BeamError> {
        let servers = self.servers.read().expect("servers lock");
        let context = servers
            .get(&server_id)
            .ok_or(BeamError::UnknownServer { server_id })?;
        // Built by hand: the generated client cannot name an operation's URL
        // without sending the request, and the in-app browser sends this one
        // (getkono/spargen#1067).
        context.record.absolute_url("/v1/auth/login?redirect=/")
    }

    /// Hand over a cookie lifted from the in-app browser and verify it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] if the server is not registered,
    /// [`BeamError::Unauthenticated`] when the server rejects the cookie or the
    /// user signed out while it was being confirmed, [`BeamError::Storage`]
    /// when the confirmed session cannot be stored, and the
    /// [request failures](BeamClient#request-failures) of `GET /v1/me`.
    pub async fn complete_login(
        &self,
        server_id: String,
        session_cookie: String,
    ) -> Result<UserSummary, BeamError> {
        self.adopt_session(&server_id, session_cookie).await
    }

    /// Begin signing in without a browser, by the OAuth 2.0 device
    /// authorization grant (ADR-0017).
    ///
    /// Show the prompt's user code and verification address, then call
    /// [`Self::poll_device_login`] every `interval_secs` until it signs in or
    /// fails. The pacing is the caller's: the core has no timer, and the
    /// server refuses a poll that comes too soon anyway.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Server`] with status 501, not retryable, when the
    /// server's identity provider does not offer the grant -- the cue to fall
    /// back to [`Self::login_url`] and [`Self::complete_login`]. Otherwise
    /// [`BeamError::UnknownServer`] if the server is not registered, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn start_device_login(
        &self,
        server_id: String,
    ) -> Result<DeviceLoginPrompt, BeamError> {
        let client = self.client_for(&server_id)?;
        let started = self
            .send(&server_id, client.start_device_login())
            .await?
            .into_inner();
        Ok(DeviceLoginPrompt {
            device_handle: started.device_handle,
            user_code: started.user_code,
            verification_uri: started.verification_uri,
            verification_uri_complete: started.verification_uri_complete,
            expires_in_secs: u64::try_from(started.expires_in_secs).unwrap_or(0),
            interval_secs: u32::try_from(started.interval_secs).unwrap_or(u32::MAX),
        })
    }

    /// Poll a device login once.
    ///
    /// On approval the issued credential goes through exactly what a lifted
    /// cookie does -- installed, confirmed against `/v1/me`, persisted -- so a
    /// device session and a browser one are indistinguishable afterwards.
    ///
    /// # Errors
    ///
    /// Every error ends the flow except a retryable one: [`BeamError::Forbidden`]
    /// when the user refused (or the account is disabled),
    /// [`BeamError::Server`] with status 410 when the code expired, and
    /// [`BeamError::BadRequest`] when the handle names no open flow. Once
    /// approved, it fails as [`Self::complete_login`] does. Otherwise
    /// [`BeamError::UnknownServer`] if the server is not registered, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn poll_device_login(
        &self,
        server_id: String,
        device_handle: String,
    ) -> Result<DeviceLoginStep, BeamError> {
        let client = self.client_for(&server_id)?;
        let body = crate::api::types::DeviceLoginPoll { device_handle };
        let answer = self
            .send(&server_id, client.poll_device_login(&body))
            .await?
            .into_inner();
        match answer {
            crate::api::PollDeviceLoginResponse::Status202(pending) => {
                Ok(DeviceLoginStep::Waiting {
                    interval_secs: u32::try_from(pending.interval_secs).unwrap_or(u32::MAX),
                    slow_down: pending.status == crate::api::types::DeviceLoginWait::SlowDown,
                })
            }
            crate::api::PollDeviceLoginResponse::Status200(complete) => {
                let user = self
                    .adopt_session(&server_id, complete.session_token)
                    .await?;
                Ok(DeviceLoginStep::SignedIn { user })
            }
        }
    }

    /// Take a `beam_session` value the server issued, however it was
    /// obtained, and make it this server's session: install it, confirm it
    /// against `/v1/me`, and persist it only once confirmed.
    async fn adopt_session(
        &self,
        server_id: &str,
        session_cookie: String,
    ) -> Result<UserSummary, BeamError> {
        let server_id = server_id.to_owned();
        let captured = SecretString::from(session_cookie.clone());

        let effects = self.apply(&server_id, SessionEvent::LoginStarted)?;
        self.perform(&server_id, &effects, None).await?;

        // `CookieCaptured` is what registers the credential, so the `/v1/me`
        // below is a secured call the client will actually build.
        let effects = self.apply(&server_id, SessionEvent::CookieCaptured)?;
        self.perform(&server_id, &effects, Some(&captured)).await?;

        match self.fetch_me(&server_id).await {
            Ok(user) => {
                // `PersistCookie`, performed here because this is the only
                // place that holds the cookie the server has just confirmed.
                // Written before the transition, so a server that reads as
                // signed in always has its session on disk; the identity goes
                // with it, which is what a cold start restores from.
                self.storage
                    .put_secret(session_key(&server_id), session_cookie)
                    .await?;
                self.storage
                    .put_secret(identity_key(&server_id), encode_identity(&user)?)
                    .await?;
                // Whatever was stranded under this key has just been
                // overwritten by a cookie the server has confirmed.
                self.forget_abandoned(&server_id);
                let effects = self.apply(
                    &server_id,
                    SessionEvent::IdentityConfirmed(Box::new(user.clone())),
                )?;
                if !effects.contains(&SessionEffect::PersistCookie) {
                    // The machine ignored the confirmation: the user signed
                    // out while `/v1/me` was in flight. Whatever this sign-in wrote is withdrawn, so a
                    // cold start does not sign the device back in. A refused
                    // delete is swallowed: the answer is still that nobody is
                    // signed in, and the session is marked abandoned so a
                    // restore drops it instead of signing back in with it.
                    if let Err(error) = self.clear_stored_session(&server_id).await {
                        tracing::warn!(
                            server_id,
                            %error,
                            "could not withdraw a sign-in confirmed after sign-out"
                        );
                    }
                    return Err(BeamError::Unauthenticated);
                }
                self.perform(&server_id, &effects, Some(&captured)).await?;
                Ok(user)
            }
            Err(error) => {
                // `IdentityRejected` withdraws and deletes the cookie. That is
                // this flow rejecting its own unconfirmed credential, not the
                // 401 path expiring an established session -- which is why it
                // is reached by the event rather than by whatever `fetch_me`
                // happened to return.
                let effects = self.apply(&server_id, SessionEvent::IdentityRejected)?;
                self.perform(&server_id, &effects, None).await?;
                Err(error)
            }
        }
    }

    /// End the session locally, and best-effort on the server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] if the server is not registered,
    /// and [`BeamError::Storage`] when the cookie cannot be cleared. A failed
    /// remote revoke is deliberately not an error: a user who taps sign out
    /// with no signal is still signed out on this device.
    pub async fn logout(&self, server_id: String) -> Result<(), BeamError> {
        // Read together, before the transition: `LogoutRequested` withdraws
        // the cookie, and the cookie is what names the session to revoke.
        let (client, session) = self.with_context(&server_id, |context| {
            (context.client.clone(), context.cookie.get())
        })?;
        // `RevokeRemoteSession`. `POST /v1/logout` takes the session as an
        // optional `beam_session` cookie *parameter* rather than through the
        // `BeamSession` scheme, so the generated client attaches the
        // registered credential only when it is passed here explicitly --
        // without it the server answers 204 and revokes nothing.
        let params = crate::api::LogoutParams {
            beam_session: session.map(|cookie| cookie.expose_secret().to_owned()),
            beam_oidc_state: None,
        };
        let _ = client.logout(Some(params)).await;

        let effects = self.apply(&server_id, SessionEvent::LogoutRequested)?;
        self.perform(&server_id, &effects, None).await
    }

    /// The current authentication state of a server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] if it is not registered.
    pub fn session_state(&self, server_id: String) -> Result<SessionState, BeamError> {
        let servers = self.servers.read().expect("servers lock");
        servers
            .get(&server_id)
            .map(|context| context.state.clone())
            .ok_or(BeamError::UnknownServer { server_id })
    }

    /// Accept a server certificate the verifier turned away.
    ///
    /// Called only after the user has been shown the certificate and has
    /// agreed to it: nothing here decides to trust anything on their behalf.
    /// The decision takes effect on the next handshake without rebuilding the
    /// HTTP client, so the connection pool survives.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] when the server is not registered,
    /// or [`BeamError::Storage`] when the decision cannot be persisted.
    pub async fn trust_certificate(
        &self,
        server_id: String,
        fingerprint: String,
    ) -> Result<(), BeamError> {
        let record = {
            let mut servers = self.servers.write().expect("servers lock");
            let context = servers
                .get_mut(&server_id)
                .ok_or_else(|| BeamError::UnknownServer {
                    server_id: server_id.clone(),
                })?;
            context.trust.trust(&fingerprint);
            if !context.record.trusted_fingerprints.contains(&fingerprint) {
                context.record.trusted_fingerprints.push(fingerprint);
            }
            context.record.clone()
        };
        self.persist_record(&record).await
    }

    /// Withdraw trust from every certificate accepted for a server.
    ///
    /// The client is rebuilt, because a live `TrustDecision` can only ever be
    /// widened -- a verifier that had already accepted a connection would
    /// otherwise keep serving it from the pool.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] when the server is not registered,
    /// [`BeamError::Storage`] when the withdrawal cannot be persisted, and
    /// [`BeamError::Network`], not retryable, when the rebuilt HTTP client
    /// cannot be built.
    pub async fn forget_certificates(&self, server_id: String) -> Result<(), BeamError> {
        let (record, cookie, state) = {
            let servers = self.servers.read().expect("servers lock");
            let context = servers
                .get(&server_id)
                .ok_or_else(|| BeamError::UnknownServer {
                    server_id: server_id.clone(),
                })?;
            let mut record = context.record.clone();
            record.trusted_fingerprints.clear();
            (record, context.cookie.get(), context.state.clone())
        };
        // The session comes across with the cookie. Withdrawing a certificate
        // is a trust decision and says nothing about who is signed in; letting
        // `install` derive the state afresh reset a signed-in server to
        // `Expired`, so forgetting a certificate told the viewer their session
        // had expired.
        self.install(record.clone(), cookie, state)?;
        self.persist_record(&record).await
    }

    /// The certificates the user has accepted for a server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::UnknownServer`] when the server is not registered.
    pub fn trusted_certificates(&self, server_id: String) -> Result<Vec<String>, BeamError> {
        self.with_context(&server_id, |context| {
            context.record.trusted_fingerprints.clone()
        })
    }

    /// Every device signed in as this user.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn sessions(&self) -> Result<Vec<DeviceSession>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_sessions()).await?;
        // A connection that is never paged: `items` is every session.
        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(DeviceSession::from_generated)
            .collect())
    }

    /// Revoke one signed-in device.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `session_id` is not a UUID, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn revoke_session(&self, session_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.delete_session(parse_uuid("session_id", &session_id)?, None),
        )
        .await?;
        Ok(())
    }

    /// End every session for this user, on every device.
    ///
    /// Unlike [`Self::logout`], a failure here *is* an error: the user asked
    /// to be signed out elsewhere, and reporting success without having done
    /// it would be a security-relevant lie.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Storage`] when this device's cookie cannot be cleared
    /// afterwards, and the [request failures](BeamClient#request-failures).
    pub async fn logout_everywhere(&self) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.logout_all()).await?;

        let effects = self.apply(&server_id, SessionEvent::LogoutRequested)?;
        self.perform(&server_id, &effects, None).await
    }

    /// The server's own health report.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn health(&self) -> Result<ServerHealth, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.get_health()).await?;
        Ok(ServerHealth::from_generated(response.into_inner()))
    }
}

impl BeamClient {
    /// A core over the platform's storage, reading time from `clock`.
    ///
    /// [`Self::new`] is this over the system clock; a test passes a
    /// `TestClock`, so the throttle interval and the queue's back-off are
    /// asserted to the second rather than raced against the wall clock.
    fn over(storage: Arc<dyn KeyValueStore>, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            clock,
            servers: RwLock::new(HashMap::new()),
            active: RwLock::new(None),
            device_profile: RwLock::new(None),
            client_kind: RwLock::new(TelemetryClientKind::default()),
            abandoned_sessions: RwLock::new(HashSet::new()),
        })
    }

    /// Build a server's context from scratch and put it in the registry.
    ///
    /// `state` is the session state the server starts in: what a cold start
    /// restored, `LoggedOut` for a newly added server, or the state carried
    /// across a rebuild of a server it already had. Deliberately only the
    /// *state*: the progress queue is persisted under the server's own key and
    /// reloads itself, and the throttle is operational rather than
    /// security-bearing, so rebuilding it costs at most one early report. The
    /// playback telemetry queue is held in memory only, so a rebuild -- a
    /// [`Self::forget_certificates`] -- drops the events not yet flushed;
    /// telemetry is best-effort by design.
    fn install(
        &self,
        record: ServerRecord,
        cookie: Option<SecretString>,
        state: SessionState,
    ) -> Result<(), BeamError> {
        let middleware = Arc::new(SessionMiddleware::new());

        // A `reqwest::Client` built without a preconfigured `ClientConfig`
        // panics with "No provider set" under the crate's TLS feature set --
        // it does not return `Err` -- so this is the only path, not a
        // hardening measure.
        let trust = Arc::new(crate::tls::TrustDecision::new(
            record.trusted_fingerprints.clone(),
        ));
        let tls = crate::tls::client_config(Arc::clone(&trust))?;
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|error| BeamError::Network {
                detail: format!("could not build an HTTP client: {error}"),
                retryable: false,
            })?;
        // The transport is handed straight to the client and kept only there:
        // nothing rebuilds the client for a session change, so its connection
        // pool and TLS session cache outlive everything but a trust change.
        // They serve the core's own API calls; the platform player and
        // downloads fetch media over their own HTTP stack.
        let transport: Arc<dyn crate::api::HttpBackend> = Arc::new(ReqwestBackend::new(http));
        let cookie = Arc::new(SessionCookie::new(cookie));
        let client = Self::client_over(transport, &middleware, &record.base_url, &cookie)?;

        let queue = Arc::new(ProgressQueue::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.clock),
            &record.id,
        ));
        let context = ServerContext {
            record,
            client,
            cookie,
            middleware,
            state,
            throttle: Arc::new(ProgressThrottle::new(Arc::clone(&self.clock))),
            queue,
            telemetry: Arc::new(TelemetryQueue::new()),
            trust,
        };
        self.servers
            .write()
            .expect("servers lock")
            .insert(context.record.id.clone(), context);
        Ok(())
    }

    /// The generated client, with the session middleware wrapped around
    /// whatever transport actually executes the request, and the `BeamSession`
    /// credential registered as a provider over `cookie`.
    ///
    /// Called once per server at install, and once more only when the
    /// transport itself is replaced -- a trust change, or a test's canned
    /// backend. A session change is a write to `cookie`, not a call to this.
    fn client_over(
        transport: Arc<dyn crate::api::HttpBackend>,
        middleware: &Arc<SessionMiddleware>,
        base_url: &str,
        cookie: &Arc<SessionCookie>,
    ) -> Result<GeneratedClient, BeamError> {
        let backend = MiddlewareBackend::with_middlewares(
            transport,
            vec![Arc::clone(middleware) as Arc<dyn crate::api::Middleware>],
        );
        let client =
            GeneratedClient::with_backend(Arc::new(backend), base_url).map_err(|error| {
                BeamError::InvalidServerUrl {
                    detail: error.to_string(),
                }
            })?;
        Ok(client.with_credential(SESSION_SCHEME, session_credential(cookie)))
    }

    /// Re-point one server's client at a canned transport, keeping its
    /// middleware, session and queue. Tests only: the seam that lets the façade
    /// see a server's answer without a listener.
    ///
    /// The client is rebuilt the way `install` builds it, over the same cookie
    /// holder -- so a test sees exactly the credential a device would send, or
    /// its absence.
    #[cfg(test)]
    fn use_transport(
        &self,
        server_id: &str,
        transport: Arc<dyn crate::api::HttpBackend>,
    ) -> Result<(), BeamError> {
        self.with_context_mut(server_id, |context| {
            context.client = Self::client_over(
                transport,
                &context.middleware,
                &context.record.base_url,
                &context.cookie,
            )?;
            Ok(())
        })?
    }

    /// The trust decision one server's TLS verifier reads and records into.
    /// Tests only: a canned handshake runs the real verifier over it, so a
    /// rejection reaches the façade the way a device's would.
    #[cfg(test)]
    fn trust_for(&self, server_id: &str) -> Result<Arc<crate::tls::TrustDecision>, BeamError> {
        self.with_context(server_id, |context| Arc::clone(&context.trust))
    }

    /// Used only by [`Self::use_transport`]: nothing in production mutates a
    /// context in place any more, because the session lives behind its own
    /// handle rather than in the client.
    #[cfg(test)]
    fn with_context_mut<T>(
        &self,
        server_id: &str,
        action: impl FnOnce(&mut ServerContext) -> T,
    ) -> Result<T, BeamError> {
        let mut servers = self.servers.write().expect("servers lock");
        servers
            .get_mut(server_id)
            .map(action)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })
    }

    fn with_context<T>(
        &self,
        server_id: &str,
        action: impl FnOnce(&ServerContext) -> T,
    ) -> Result<T, BeamError> {
        let servers = self.servers.read().expect("servers lock");
        servers
            .get(server_id)
            .map(action)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })
    }

    /// Drive the session machine, and hand back what it asked for.
    ///
    /// The effects are returned rather than dropped because the machine is the
    /// only thing that knows which transitions are destructive. Discarding
    /// them meant the credential was withdrawn and the stored cookie deleted
    /// by whichever call site felt like it, from states the machine says must
    /// be left alone -- a 401 racing a sign-in among them.
    fn apply(&self, server_id: &str, event: SessionEvent) -> Result<Vec<SessionEffect>, BeamError> {
        let mut servers = self.servers.write().expect("servers lock");
        let context = servers
            .get_mut(server_id)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })?;
        let (next, effects) = crate::session::transition(&context.state, event);
        context.state = next;
        Ok(effects)
    }

    /// Perform the effects one transition emitted.
    ///
    /// The one place `InstallCookie` and `ClearCookie` happen, so the
    /// credential comes off the client and the cookie leaves the platform
    /// keystore exactly when the machine says and never otherwise.
    ///
    /// `captured` is the cookie the event carried, where it carried one: only
    /// `CookieCaptured` and `StoredSessionRestored` ask for a cookie to be
    /// installed, and both are applied by a caller that is holding it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when `ClearCookie` cannot be performed,
    /// and [`BeamError::UnknownServer`] for a server that has been removed.
    /// Whether that is fatal is the caller's to decide: a sign-out propagates
    /// it, while the 401 path swallows it -- see [`Self::fail`].
    async fn perform(
        &self,
        server_id: &str,
        effects: &[SessionEffect],
        captured: Option<&SecretString>,
    ) -> Result<(), BeamError> {
        for effect in effects {
            match effect {
                // Installing means installing what the event carried; asking
                // for a cookie the caller did not supply is asking for none.
                SessionEffect::InstallCookie(install) => {
                    let cookie = (*install).then(|| captured.cloned()).flatten();
                    self.with_context(server_id, |context| context.set_session(cookie))?;
                }
                SessionEffect::ClearCookie => {
                    self.clear_stored_session(server_id).await?;
                }
                // Performed by the call site, which has what the effect needs
                // and this does not: the confirmed cookie for `PersistCookie`,
                // and a client to make the request with for the other two.
                SessionEffect::PersistCookie
                | SessionEffect::VerifyIdentity
                | SessionEffect::RevokeRemoteSession => {}
                // Nothing implements this. The core has no observer channel --
                // the foreign side polls `session_state` -- so there is no
                // handler to call, and inventing one is a change to the FFI
                // surface rather than a fix to this one. Named here so the
                // gap is visible at the point it would be filled.
                SessionEffect::NotifyObserver => {}
            }
        }
        Ok(())
    }

    /// Delete this server's stored session -- its cookie and the identity a
    /// cold start restores from -- or, where the keystore refuses, mark it
    /// abandoned so `restore` does not sign the device back in with it.
    ///
    /// Every path that withdraws a stored session goes through here, so no
    /// failed delete can leave a session on disk unmarked.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when either delete fails; the mark is
    /// set by then, so a caller may swallow it.
    async fn clear_stored_session(&self, server_id: &str) -> Result<(), BeamError> {
        // Removing an absent key succeeds, so this asks no question the
        // caller would have to answer twice. The cookie goes first: it is the
        // credential, and an identity left without one restores nothing.
        let cleared = match self.storage.remove_secret(session_key(server_id)).await {
            Ok(()) => self.storage.remove_secret(identity_key(server_id)).await,
            Err(error) => Err(error),
        };
        match cleared {
            Ok(()) => {
                self.forget_abandoned(server_id);
                Ok(())
            }
            Err(error) => {
                self.abandon_session(server_id);
                Err(error.into())
            }
        }
    }

    /// Note that this server's stored cookie could not be deleted.
    fn abandon_session(&self, server_id: &str) {
        self.abandoned_sessions
            .write()
            .expect("abandoned sessions lock")
            .insert(server_id.to_owned());
    }

    /// Forget that note: the key has since been deleted or overwritten.
    fn forget_abandoned(&self, server_id: &str) {
        self.abandoned_sessions
            .write()
            .expect("abandoned sessions lock")
            .remove(server_id);
    }

    /// Whether this server's stored cookie is one the device failed to delete.
    fn is_abandoned(&self, server_id: &str) -> bool {
        self.abandoned_sessions
            .read()
            .expect("abandoned sessions lock")
            .contains(server_id)
    }

    fn client_for(&self, server_id: &str) -> Result<GeneratedClient, BeamError> {
        self.with_context(server_id, |context| context.client.clone())
    }

    fn record_for(&self, server_id: &str) -> Result<ServerRecord, BeamError> {
        self.with_context(server_id, |context| context.record.clone())
    }

    /// The active server's id, client and record together.
    ///
    /// Every catalog call needs all three, and taking them in one shot keeps
    /// the servers lock held for a single statement rather than three.
    fn active_context(&self) -> Result<(String, GeneratedClient, ServerRecord), BeamError> {
        let server_id = self.require_active()?;
        let (client, record) = self.with_context(&server_id, |context| {
            (context.client.clone(), context.record.clone())
        })?;
        Ok((server_id, client, record))
    }

    fn require_active(&self) -> Result<String, BeamError> {
        self.active
            .read()
            .expect("active lock")
            .clone()
            .ok_or(BeamError::NoActiveServer)
    }

    fn summary(&self, server_id: &str) -> Result<ServerSummary, BeamError> {
        self.list_servers()?
            .into_iter()
            .find(|summary| summary.id == server_id)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })
    }

    /// Name a failed request in the core's taxonomy.
    ///
    /// A certificate the verifier turned away on this server is
    /// [`BeamError::UntrustedCertificate`], whatever the transport said. A
    /// failure that carried a response is classified from its status and
    /// problem document by [`classify`] -- so a 404 is a `NotFound` a viewer
    /// can be told about, and a 400 is not offered a retry. Only a failure
    /// that never reached a response is a `Network`.
    ///
    /// Classification only: it drives no session and performs no effect. A 401
    /// is the session machine's business and is handled by [`Self::fail`],
    /// which is this function's one caller -- so an answered 401 that reaches
    /// here is one the machine declined to act on, and `classify` gives it the
    /// honest name of [`BeamError::Unauthenticated`].
    fn map_error(&self, server_id: &str, failure: &TransportFailure) -> BeamError {
        // Checked before the generic network error, because a rejected
        // certificate reaches this point as an ordinary transport failure
        // whose text names neither the certificate nor what to do about it.
        if let Ok(Some((host, details))) =
            self.with_context(server_id, |context| context.trust.take_rejection())
        {
            return BeamError::UntrustedCertificate { host, details };
        }

        match failure.kind {
            FailureKind::Answered(status) => {
                classify(status, failure.problem.as_ref(), failure.retry_after_secs)
            }
            FailureKind::Unreachable => BeamError::Network {
                detail: failure.message.clone(),
                retryable: true,
            },
            // Not `Network { retryable: false }`: this is not the network, and
            // `Protocol` is the variant that already means "the response did
            // not match the contract the client was generated from".
            FailureKind::Malformed => BeamError::Protocol {
                detail: failure.message.clone(),
            },
            // The request never left, because the `BeamSession` provider had
            // no cookie to give it. That the credential is what was missing is
            // the client's own answer, downcast from the provider's
            // `AuthError` -- not inferred from what this façade happens to
            // hold. *Which* of the two unauthenticated answers to give is the
            // session's business, though, and the UI treats them differently:
            // a device that never signed in is sent to a cold sign-in, one
            // whose session the server rejected keeps its work in progress.
            // Both are cured by signing in, which is what lets a progress
            // sample wait for the user instead of being dropped.
            FailureKind::NoCredential => {
                match self.with_context(server_id, |context| context.state.clone()) {
                    Ok(SessionState::Expired) => BeamError::SessionExpired,
                    _ => BeamError::Unauthenticated,
                }
            }
            // Anything else that never left is a request that genuinely could
            // not be built: the base URL was validated at install and
            // serialising the crate's own request types does not fail, so this
            // is a contract failure rather than a session one, and no amount
            // of signing in changes it.
            FailureKind::Unbuildable => BeamError::Protocol {
                detail: failure.message.clone(),
            },
        }
    }

    /// Run one generated-client call against `server_id`, and turn its
    /// failure into the core's error.
    ///
    /// The single shape every operation takes, so that no call can reach the
    /// server without the problem scope open ([`TransportFailure::capture`])
    /// or fail without the session machine hearing about it ([`Self::fail`]).
    async fn send<T, E: std::fmt::Display>(
        &self,
        server_id: &str,
        request: impl std::future::Future<Output = Result<T, crate::api::Error<E>>>,
    ) -> Result<T, BeamError> {
        match TransportFailure::capture(request).await {
            Ok(value) => Ok(value),
            Err(failure) => Err(self.fail(server_id, &failure).await),
        }
    }

    /// Tell the session machine what the server said, perform whatever it
    /// asks for, and name the failure.
    ///
    /// The 401 the middleware saw is fed in as `UnauthorizedObserved` and the
    /// machine decides: from `Authenticated` that is the session expiring, and
    /// the credential comes off the client and the cookie out of the keystore.
    /// From any other state it is a late answer to a cancelled request, the
    /// machine emits nothing, and nothing is destroyed -- which is the whole
    /// point of asking it rather than acting on the flag directly.
    ///
    /// The deletion is keyed on the emitted `ClearCookie`, not on the error
    /// this returns. Keying it on the error meant every failing call deleted
    /// the key again while the device sat in `Expired`, and meant a path where
    /// nothing was ever sent could delete a cookie no server had rejected.
    async fn fail(&self, server_id: &str, failure: &TransportFailure) -> BeamError {
        let rejected = self
            .with_context(server_id, |context| context.middleware.take_unauthorized())
            .unwrap_or(false);
        if rejected {
            let effects = self
                .apply(server_id, SessionEvent::UnauthorizedObserved)
                .unwrap_or_default();
            // `InstallCookie(false)` is the credential coming off the client,
            // so the next secured call is refused here instead of sent with a
            // cookie the server has already rejected -- and so
            // `server_http_config` stops handing that cookie to the player.
            let expired = effects.contains(&SessionEffect::InstallCookie(false));
            if let Err(error) = self.perform(server_id, &effects, None).await {
                // Swallowed: the caller's error is the expired session, which
                // stands, and the credential is already off the client. The
                // cookie the server rejected is still in the keystore, but
                // `ClearCookie` has marked it abandoned, so a restore does not
                // send it again.
                tracing::warn!(server_id, %error, "could not delete the rejected session cookie");
            }
            if expired {
                return BeamError::SessionExpired;
            }
        }
        self.map_error(server_id, failure)
    }

    async fn fetch_me(&self, server_id: &str) -> Result<UserSummary, BeamError> {
        let client = self.client_for(server_id)?;
        let response = self.send(server_id, client.get_current_user()).await?;
        let me = response.into_inner();
        Ok(UserSummary {
            id: me.id.to_string(),
            display_name: me.display_name,
            email: me.email,
            is_admin: me.is_admin,
            avatar_url: me.avatar_url,
        })
    }

    /// The session a cold start can restore for `server_id`: its stored
    /// cookie, and who the server confirmed it belongs to.
    ///
    /// Both or neither. A cookie with no readable identity beside it -- one
    /// stored before identities were, or whose identity was lost -- is not
    /// restored, because a signed-in state has to say who is signed in and
    /// inventing an identity would grant whatever the invention claimed. It
    /// stays where it is; the next sign-in overwrites it, and a sign-out
    /// deletes it.
    async fn stored_session(
        &self,
        server_id: &str,
    ) -> Result<Option<(SecretString, UserSummary)>, BeamError> {
        let Some(cookie) = self.storage.get_secret(session_key(server_id)).await? else {
            return Ok(None);
        };
        let identity = self
            .storage
            .get_secret(identity_key(server_id))
            .await?
            .and_then(|raw| serde_json::from_str::<StoredIdentity>(&raw).ok());
        Ok(identity.map(|stored| (SecretString::from(cookie), UserSummary::from(stored))))
    }

    async fn persist_record(&self, record: &ServerRecord) -> Result<(), BeamError> {
        let encoded = serde_json::to_string(record).map_err(|error| BeamError::Storage {
            detail: error.to_string(),
        })?;
        self.storage
            .put(format!("servers/{}", record.id), encoded)
            .await?;
        self.persist_index().await
    }

    async fn persist_index(&self) -> Result<(), BeamError> {
        let ids: Vec<String> = self
            .servers
            .read()
            .expect("servers lock")
            .keys()
            .cloned()
            .collect();
        let encoded = serde_json::to_string(&ids).map_err(|error| BeamError::Storage {
            detail: error.to_string(),
        })?;
        self.storage
            .put(SERVER_INDEX_KEY.to_owned(), encoded)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
