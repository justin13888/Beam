//! The surface Kotlin and Swift actually call.
//!
//! Everything here is a thin shell over the modules beside it. The logic lives
//! in those modules, where it is testable as plain Rust; this file only
//! marshals. Keeping the split strict is what stops UniFFI's generated
//! scaffolding from dominating the crate's coverage, and it means a Kotlin
//! integration test and a Rust unit test are exercising the same code.

use crate::api::Client as GeneratedClient;
use crate::api::{MiddlewareBackend, ReqwestBackend};
use crate::capability::{
    DeviceProfile, MediaSourceView, QualityPolicy, RebufferDetector, SourceSelection,
};
use crate::catalog::{
    AdminEvent, AdminLogEntry, AdminLogPage, AdminStatus, AdminUser, AdminUserPage, BrowseQuery,
    ContinueWatchingEntry, DeviceSession, EpisodeSummary, HistoryEntry, HistoryPage,
    LibraryFileSummary, LibrarySummary, MediaDetail, MediaPage, ScanJob, ServerHealth,
    browse_params,
};
use crate::clock::{Clock, SystemClock};
use crate::error::BeamError;
use crate::ports::kv::KeyValueStore;
use crate::progress::{
    ProgressOutcome, ProgressQueue, ProgressThrottle, QueuedProgress, ThrottleDecision,
};
use crate::servers::{ServerRecord, normalize_base_url, server_id_for};
use crate::session::{
    DeviceLoginPrompt, DeviceLoginStep, SessionEffect, SessionEvent, SessionState, UserSummary,
};
use crate::telemetry::{
    PlaybackEvent, PlaybackTelemetrySummary, StartFailureCause, SwitchTrigger, TELEMETRY_DISABLED,
    TelemetryClientKind, TelemetryFlush, TelemetryQueue, batch_body,
};
use crate::transport::{ABOUT_BLANK, FailureKind, SessionMiddleware, TransportFailure, classify};
use secrecy::{ExposeSecret, SecretString};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Storage key holding the list of known server ids.
const SERVER_INDEX_KEY: &str = "servers/index";
/// Storage key holding the currently selected server id.
const ACTIVE_SERVER_KEY: &str = "servers/active";
/// The page size [`BeamClient::library_files`] walks a library's files at:
/// the largest `GET /v1/libraries/{id}/files` takes, so the fewest round
/// trips.
const LIBRARY_FILES_PAGE: i64 = 500;
/// The fragment of the problem type `GET /v1/admin/libraries/{id}/scan`
/// answers with when the library has not been scanned since the server
/// started.
const SCAN_NOT_FOUND: &str = "#scan-not-found";

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

/// How the platform reaches one server, independent of any single file.
///
/// Downloads need this and cannot use [`PlaybackHttpConfig`]: Media3's
/// `DownloadManager` is built with one `DataSource.Factory` for every download
/// it will ever perform, so a config carrying one specific file's URL is the
/// wrong shape entirely. The credential and the trust decision are per-server;
/// only the URL is per-file.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerHttpConfig {
    /// The server's normalised origin.
    pub base_url: String,
    /// Headers to attach, including the session cookie.
    pub headers: HashMap<String, String>,
    /// Certificates the user has explicitly accepted for this server, as
    /// whole-certificate SHA-256 digests in colon-grouped uppercase hex.
    pub trusted_fingerprints: Vec<String>,
    /// The host those fingerprints apply to.
    pub host: String,
}

/// Everything the platform player needs to fetch bytes itself.
///
/// Media3 does its own buffering and range requests, so the core must not sit
/// in that path -- it would mean proxying the whole stream through the FFI
/// boundary for no benefit. Instead the core hands over the URL and the
/// credential, and the platform's own HTTP stack does the transfer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PlaybackHttpConfig {
    /// Absolute URL to stream from.
    pub url: String,
    /// Headers to attach, including the session cookie.
    pub headers: HashMap<String, String>,
    /// Certificates the user has explicitly accepted for this server, as
    /// whole-certificate SHA-256 digests in colon-grouped uppercase hex.
    /// Empty when public trust already suffices.
    ///
    /// Deliberately *not* OkHttp `CertificatePinner` values. `CertificatePinner`
    /// runs after the platform trust manager has already accepted the chain,
    /// so it can only narrow trust, never widen it -- it cannot rescue the
    /// self-signed certificate a LAN server presents. The platform player
    /// needs a trust manager that admits these exact certificates, which is
    /// what the whole-certificate digest identifies. It is also the digest the
    /// user was shown when they accepted it.
    pub trusted_fingerprints: Vec<String>,
    /// The host those fingerprints apply to. A fingerprint is permission to
    /// trust one certificate for one server, never a wildcard.
    pub pinned_host: String,
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

/// The sustained-rebuffer detector, as the platform player holds it.
///
/// One per player. Every timestamp is the platform's monotonic clock in
/// milliseconds (Android's `elapsedRealtime`), never wall-clock time, which
/// can jump. Each call returns whether rebuffering on the current file is
/// sustained; the rule itself is [`RebufferDetector`]'s.
#[derive(Debug, Default, uniffi::Object)]
pub struct RebufferMonitor {
    detector: Mutex<RebufferDetector>,
}

#[uniffi::export]
impl RebufferMonitor {
    /// A monitor that has seen nothing.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Playback stalled mid-stream. Never called for the buffering that
    /// starts a file or follows a seek.
    pub fn stall_started(&self, now_ms: u64) -> bool {
        self.detector
            .lock()
            .expect("detector lock")
            .stall_started(now_ms)
    }

    /// Playback resumed after a stall.
    pub fn stall_ended(&self, now_ms: u64) -> bool {
        self.detector
            .lock()
            .expect("detector lock")
            .stall_ended(now_ms)
    }

    /// Re-judge without a new event, so a stall that never ends still trips.
    pub fn poll(&self, now_ms: u64) -> bool {
        self.detector.lock().expect("detector lock").poll(now_ms)
    }

    /// Forget everything, because a different file is now playing.
    pub fn reset(&self) {
        self.detector.lock().expect("detector lock").reset();
    }
}

/// The core, as the foreign side holds it.
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

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// Build a core over the platform's storage.
    #[uniffi::constructor]
    #[must_use]
    pub fn new(storage: Arc<dyn KeyValueStore>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            clock: Arc::new(SystemClock),
            servers: RwLock::new(HashMap::new()),
            active: RwLock::new(None),
            device_profile: RwLock::new(None),
            client_kind: RwLock::new(TelemetryClientKind::default()),
            abandoned_sessions: RwLock::new(HashSet::new()),
        })
    }

    /// Load the server registry and any stored sessions.
    ///
    /// Sessions are restored optimistically: the first real request either
    /// confirms one or reports it expired, which keeps app start off the
    /// network.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the registry cannot be read.
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
            let cookie = if self.is_abandoned(&id) {
                // A cookie the device could not delete is not a session: the
                // server rejected it, and registering it again would put it
                // straight back on the wire. The delete is retried first,
                // because the keystore that refused it may since have
                // unlocked; the cookie is dropped either way.
                if self
                    .storage
                    .remove_secret(format!("session/{id}"))
                    .await
                    .is_ok()
                {
                    self.forget_abandoned(&id);
                }
                None
            } else {
                self.storage
                    .get_secret(format!("session/{id}"))
                    .await?
                    .map(SecretString::from)
            };
            self.install(record, cookie, None)?;
        }

        *self.active.write().expect("active lock") =
            self.storage.get(ACTIVE_SERVER_KEY.to_owned()).await?;
        self.list_servers()
    }

    /// Add a server, or return the existing one if it is already known.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::InvalidServerUrl`] for an unusable address, or
    /// [`BeamError::Storage`] when the registry cannot be written.
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
            self.install(record.clone(), None, None)?;
            self.persist_record(&record).await?;
        }

        self.select_server(id.clone()).await?;
        self.summary(&id)
    }

    /// Every known server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] on an unreadable registry.
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
    /// Returns [`BeamError::UnknownServer`] if it is not registered.
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
        self.storage
            .remove_secret(format!("session/{server_id}"))
            .await?;
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
    /// Returns [`BeamError::UnknownServer`] if the server is not registered.
    pub fn login_url(&self, server_id: String) -> Result<String, BeamError> {
        let servers = self.servers.read().expect("servers lock");
        let context = servers
            .get(&server_id)
            .ok_or(BeamError::UnknownServer { server_id })?;
        context.record.absolute_url("/v1/auth/login?redirect=/")
    }

    /// Hand over a cookie lifted from the in-app browser and verify it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Unauthenticated`] when the server rejects it.
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
    /// back to [`Self::login_url`] and [`Self::complete_login`].
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
    /// [`BeamError::BadRequest`] when the handle names no open flow.
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
                self.storage
                    .put_secret(format!("session/{server_id}"), session_cookie)
                    .await?;
                // Whatever was stranded under this key has just been
                // overwritten by a cookie the server has confirmed.
                self.forget_abandoned(&server_id);
                let effects = self.apply(
                    &server_id,
                    SessionEvent::IdentityConfirmed(Box::new(user.clone())),
                )?;
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
    /// Returns [`BeamError::Storage`] when the cookie cannot be cleared. A
    /// failed remote revoke is deliberately not an error: a user who taps sign
    /// out with no signal is still signed out on this device.
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

    /// Tell the core what this device can decode.
    ///
    /// Assembled on the foreign side, where `MediaCodecList` lives.
    pub fn set_device_profile(&self, profile: DeviceProfile) {
        *self.device_profile.write().expect("profile lock") = Some(profile);
    }

    /// The playable sources for a movie or episode.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn media_sources(&self, media_id: String) -> Result<Vec<MediaSourceView>, BeamError> {
        let server_id = self.require_active()?;
        let client = self.client_for(&server_id)?;
        let record = self.record_for(&server_id)?;

        let response = self
            .send(
                &server_id,
                client.get_media_sources(parse_uuid("media_id", &media_id)?),
            )
            .await?;

        response
            .into_inner()
            .items
            .into_iter()
            .map(|source| to_view(source, &record))
            .collect()
    }

    /// Choose a source for a title, given the device profile and a policy.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoPlayableSource`]-shaped detail through
    /// [`BeamError::NotFound`] when nothing can play here.
    pub async fn select_playback_source(
        &self,
        media_id: String,
        policy: QualityPolicy,
    ) -> Result<SourceSelection, BeamError> {
        let sources = self.media_sources(media_id).await?;
        let profile = self.require_device_profile()?;

        crate::capability::select_source(&sources, &profile, &policy).map_err(|rejections| {
            let detail = rejections.first().map_or_else(
                || "This title has no playable files".to_owned(),
                |first| first.detail.clone(),
            );
            BeamError::NotFound {
                detail,
                code: ABOUT_BLANK.to_owned(),
            }
        })
    }

    /// A smaller file of the same title to fall back to, after the platform's
    /// [`RebufferMonitor`] judged rebuffering on `current_file_id` sustained.
    ///
    /// `None` means there is nothing smaller this device can play, which is
    /// an answer rather than a failure: playback continues on the current
    /// file. See [`crate::capability::select_downgrade`] for how it chooses.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures, and refuses with
    /// [`BeamError::BadRequest`] before the device profile has been set.
    pub async fn select_downgrade_source(
        &self,
        media_id: String,
        current_file_id: String,
    ) -> Result<Option<SourceSelection>, BeamError> {
        let sources = self.media_sources(media_id).await?;
        let profile = self.require_device_profile()?;
        Ok(crate::capability::select_downgrade(
            &sources,
            &current_file_id,
            &profile,
        ))
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
    /// Returns [`BeamError::UnknownServer`] when the server is not registered.
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
        self.install(record.clone(), cookie, Some(state))?;
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

    /// Everything the platform player needs to stream a file itself.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Unauthenticated`] when there is no session.
    pub fn playback_config(&self, file_id: String) -> Result<PlaybackHttpConfig, BeamError> {
        let server = self.server_http_config()?;
        let ServerHttpConfig {
            base_url: _,
            headers,
            trusted_fingerprints,
            host,
        } = server;

        let server_id = self.require_active()?;
        let url = self.with_context(&server_id, |context| {
            context
                .record
                .absolute_url(&format!("/v1/files/{file_id}/stream"))
        })??;

        Ok(PlaybackHttpConfig {
            url,
            headers,
            trusted_fingerprints,
            pinned_host: host,
        })
    }

    /// The credential and trust decision for the active server.
    ///
    /// Separate from [`Self::playback_config`] because Media3's download
    /// manager is constructed once with a single data-source factory, long
    /// before any particular file is chosen.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Unauthenticated`] when there is no session.
    pub fn server_http_config(&self) -> Result<ServerHttpConfig, BeamError> {
        let server_id = self.require_active()?;
        let servers = self.servers.read().expect("servers lock");
        let context = servers.get(&server_id).ok_or(BeamError::UnknownServer {
            server_id: server_id.clone(),
        })?;

        let cookie = context.cookie.get().ok_or(BeamError::Unauthenticated)?;
        let cookie = cookie.expose_secret();
        let base_url = context.record.base_url.clone();
        let host = url::Url::parse(&base_url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_default();

        let mut headers = HashMap::new();
        headers.insert(
            "Cookie".to_owned(),
            format!("{}={cookie}", crate::transport::SESSION_COOKIE),
        );

        Ok(ServerHttpConfig {
            base_url,
            headers,
            trusted_fingerprints: context.record.trusted_fingerprints.clone(),
            host,
        })
    }

    // -- catalog ----------------------------------------------------------

    /// One page of the catalog, filtered and sorted.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] for a query the server would reject,
    /// and propagates transport failures.
    pub async fn browse_media(&self, query: BrowseQuery) -> Result<MediaPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let response = self
            .send(&server_id, client.browse_media(browse_params(&query)?))
            .await?;
        Ok(MediaPage::from_generated(response.into_inner(), &record))
    }

    /// Everything a detail screen shows for one title.
    ///
    /// Read from the server every time, never cached: the detail carries the
    /// viewer's watched state (issue #188), which playback and a watched mark
    /// change under it, so a cached copy would show an episode just finished
    /// as unwatched.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn media_detail(&self, media_id: String) -> Result<MediaDetail, BeamError> {
        let wire_media_id = parse_uuid("media_id", &media_id)?;
        let (server_id, client, record) = self.active_context()?;
        let response = self
            .send(&server_id, client.get_media_detail(wire_media_id))
            .await?;
        Ok(MediaDetail::from_generated(response.into_inner(), &record))
    }

    /// Mark a movie, an episode, a season or a whole show watched.
    ///
    /// A season or a show marks each of its episodes. Marking a title already
    /// watched changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `id` is not a UUID and
    /// [`BeamError::NotFound`] for an id the server does not know.
    pub async fn mark_watched(&self, id: String) -> Result<(), BeamError> {
        let wire_id = parse_uuid("id", &id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.mark_watched(wire_id)).await?;
        Ok(())
    }

    /// Mark a movie, an episode, a season or a whole show unwatched: its
    /// played state, play count and resume position are all forgotten.
    ///
    /// # Errors
    ///
    /// As [`Self::mark_watched`].
    pub async fn mark_unwatched(&self, id: String) -> Result<(), BeamError> {
        let wire_id = parse_uuid("id", &id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.mark_unwatched(wire_id))
            .await?;
        Ok(())
    }

    /// Every genre the catalog contains, for the explore filters.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn genres(&self) -> Result<Vec<String>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_genres()).await?;
        // A connection that is never paged: `items` is every genre.
        Ok(response.into_inner().items)
    }

    /// The episode of `show_id` that plays after `current_episode_id`.
    ///
    /// The server's answer, `EpisodeDetail::next_episode_id` (issue #188),
    /// rather than one worked out here: next-up skips episodes with no file,
    /// crosses season boundaries, and steps over the run of episodes a
    /// multi-episode file holds, and only the server knows the last of those.
    /// So every client advances to the same episode.
    ///
    /// `None` after the last episode, and for an episode that is not
    /// `show_id`'s: auto-advance never carries a viewer into another show.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] for an identifier that is not a UUID,
    /// and propagates transport and server failures.
    pub async fn up_next_in_show(
        &self,
        show_id: String,
        current_episode_id: String,
    ) -> Result<Option<EpisodeSummary>, BeamError> {
        let wire_show_id = parse_uuid("show_id", &show_id)?;
        let wire_current_id = parse_uuid("current_episode_id", &current_episode_id)?;
        let (server_id, client, record) = self.active_context()?;

        let current = self
            .send(&server_id, client.get_episode_detail(wire_current_id))
            .await?
            .into_inner();
        if current.show.id != wire_show_id {
            return Ok(None);
        }
        let Some(next_id) = current.next_episode_id else {
            return Ok(None);
        };
        let next = self
            .send(&server_id, client.get_episode_detail(next_id))
            .await?
            .into_inner();
        Ok(Some(EpisodeSummary::from_generated(&next.episode, &record)))
    }

    // -- libraries --------------------------------------------------------

    /// Every library on the server.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn libraries(&self) -> Result<Vec<LibrarySummary>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_libraries()).await?;
        // A connection that is never paged: `items` is every library.
        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(LibrarySummary::from_generated)
            .collect())
    }

    /// One library.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn library(&self, library_id: String) -> Result<LibrarySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(
                &server_id,
                client.get_library(parse_uuid("library_id", &library_id)?),
            )
            .await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// The files indexed into one library, by path.
    ///
    /// The server pages this list; the core walks every page, at the
    /// largest size the server takes, so a caller still gets the whole list.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn library_files(
        &self,
        library_id: String,
    ) -> Result<Vec<LibraryFileSummary>, BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        let mut files = Vec::new();
        let mut after = None;
        loop {
            let params = crate::api::GetLibraryFilesParams {
                first: Some(LIBRARY_FILES_PAGE),
                after: after.take(),
            };
            let page = self
                .send(
                    &server_id,
                    client.get_library_files(wire_library_id, params),
                )
                .await?
                .into_inner();
            files.extend(
                page.items
                    .into_iter()
                    .map(LibraryFileSummary::from_generated),
            );
            match page.page_info.end_cursor {
                Some(cursor) if page.page_info.has_next_page => after = Some(cursor),
                _ => break,
            }
        }
        Ok(files)
    }

    // -- playback ---------------------------------------------------------

    /// Titles to resume and next episodes to start, newest first.
    ///
    /// One round trip: every row carries what it displays (issue #188), so
    /// nothing is looked up per row.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn continue_watching(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<ContinueWatchingEntry>, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetContinueWatchingParams {
            first: limit.map(i64::from),
        };
        let response = self
            .send(&server_id, client.get_continue_watching(params))
            .await?;

        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(|item| ContinueWatchingEntry::from_generated(item, &record))
            .collect())
    }

    /// One page of watch history, newest first: `first` rows, after the
    /// page whose `end_cursor` is `after`, or from the start.
    ///
    /// Each row carries what it displays, as [`Self::continue_watching`]'s
    /// do.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn history(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<HistoryPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetHistoryParams {
            first: first.map(i64::from),
            after,
        };
        let response = self
            .send(&server_id, client.get_history(params))
            .await?
            .into_inner();

        let total = u64::try_from(response.total).unwrap_or(0);
        let end_cursor = response.page_info.end_cursor;
        let has_next_page = response.page_info.has_next_page;
        let items: Vec<HistoryEntry> = response
            .items
            .into_iter()
            .map(|item| HistoryEntry::from_generated(item, &record))
            .collect();
        Ok(HistoryPage {
            items,
            total,
            end_cursor,
            has_next_page,
        })
    }

    /// Report where the viewer is in a file.
    ///
    /// Applies the shared throttle, and persists anything that could not be
    /// sent so a lost connection does not lose the user's place.
    ///
    /// `force` bypasses the interval for pause, seek-end and player release.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `file_id` is not a UUID, and
    /// [`BeamError::Storage`] only when the retry queue itself cannot be
    /// written. A failed *send* is reported as [`ProgressOutcome::Queued`],
    /// not as an error: the position is safe, and a player should not surface
    /// a network blip as a playback failure.
    pub async fn report_progress(
        &self,
        file_id: String,
        position_secs: f64,
        duration_secs: Option<f64>,
        force: bool,
    ) -> Result<ProgressOutcome, BeamError> {
        let wire_file_id = parse_uuid("file_id", &file_id)?;
        let (server_id, client, _) = self.active_context()?;
        let (throttle, queue) = self.with_context(&server_id, |context| {
            (Arc::clone(&context.throttle), Arc::clone(&context.queue))
        })?;

        let (position, duration) =
            match throttle.decide(&file_id, position_secs, duration_secs, force) {
                ThrottleDecision::Hold {
                    next_eligible_in_secs,
                } => {
                    return Ok(ProgressOutcome::Throttled {
                        next_eligible_in_secs,
                    });
                }
                ThrottleDecision::Send {
                    position_secs,
                    duration_secs,
                } => (position_secs, duration_secs),
            };

        let body = crate::api::types::ReportProgressRequest {
            duration_secs: duration,
            position_secs: position,
        };
        match TransportFailure::capture(client.report_playback_progress(wire_file_id, &body)).await
        {
            Ok(response) => {
                queue.remove(&file_id).await?;
                Ok(ProgressOutcome::Sent {
                    position_secs: response.into_inner().position_secs,
                })
            }
            Err(failure) => {
                // The throttle already recorded this as sent, so the next
                // sample would be held back behind an interval that never
                // produced a request. Clearing it lets the retry happen.
                throttle.reset(&file_id);
                let mapped = self.fail(&server_id, &failure).await;

                // A rejected body, a forbidden file, a title that no longer
                // exists: the identical request fails identically forever.
                // Queuing one occupied a slot in a bounded queue and never
                // retired, because `enqueue` replaces the entry and resets
                // `attempts` -- so while a title played and sampled every
                // fifteen seconds, MAX_ATTEMPTS was reset before it could
                // count. `Dropped` existed for exactly this and had no
                // producer.
                //
                // Only a *permanent* refusal is dropped. An expired session,
                // a missing one, or a certificate the user has not yet trusted
                // is not retryable *now*, but the same sample is accepted the
                // moment the user signs in or trusts the certificate -- and
                // the whole point of the queue is that a resume point survives
                // exactly that kind of interruption. Those are queued, and
                // `flush_progress` sends them once the user has acted.
                if mapped.is_permanent() {
                    return Ok(ProgressOutcome::Dropped {
                        reason: mapped.to_string(),
                    });
                }

                queue
                    .enqueue(QueuedProgress {
                        file_id,
                        position_secs: position,
                        duration_secs: duration,
                        captured_at_unix: self.clock.now_unix(),
                        attempts: 0,
                        not_before_unix: self.clock.now_unix(),
                    })
                    .await?;
                let pending = queue.len().await?;
                Ok(ProgressOutcome::Queued {
                    pending: u32::try_from(pending).unwrap_or(u32::MAX),
                })
            }
        }
    }

    /// Send every queued position that is due.
    ///
    /// Called on reconnect and at app start. Returns how many were accepted.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the queue cannot be read or written,
    /// and [`BeamError::SessionExpired`] when the server rejected the session
    /// part-way through. That second one is not incidental bookkeeping: the
    /// rejection takes the credential off the client and deletes the stored
    /// cookie, so a flush that answered with a count alone signed the device
    /// out and told the caller only how many samples went. Whatever was
    /// accepted before it has already left the queue, and
    /// [`Self::pending_progress_count`] says what is still waiting.
    pub async fn flush_progress(&self) -> Result<u32, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let queue = self.with_context(&server_id, |context| Arc::clone(&context.queue))?;

        let mut sent = 0_u32;
        for entry in queue.ready().await? {
            let Ok(wire_file_id) = uuid::Uuid::parse_str(&entry.file_id) else {
                // The path parameter is a UUID, so an entry that is not one
                // was written by an older build and can never be accepted.
                // It is treated exactly like a rejected send.
                queue.record_failure(&entry.file_id, None).await?;
                break;
            };
            let body = crate::api::types::ReportProgressRequest {
                duration_secs: entry.duration_secs,
                position_secs: entry.position_secs,
            };
            match TransportFailure::capture(client.report_playback_progress(wire_file_id, &body))
                .await
            {
                Ok(_) => {
                    queue.remove(&entry.file_id).await?;
                    sent = sent.saturating_add(1);
                }
                Err(failure) => {
                    // The server's own interval, where it sent one. Passing
                    // `None` here fell back to blind exponential backoff and
                    // ignored a 429's `Retry-After` -- which this crate reads,
                    // parses, and carried as far as this line before dropping.
                    queue
                        .record_failure(&entry.file_id, failure.retry_after_secs)
                        .await?;
                    // A queue drain stops at the first failure rather than
                    // hammering an unreachable server with the whole backlog.
                    // An expiry stops it *and says so*: this is a background
                    // call, and it has just signed the device out.
                    let mapped = self.fail(&server_id, &failure).await;
                    if matches!(mapped, BeamError::SessionExpired) {
                        return Err(mapped);
                    }
                    break;
                }
            }
        }
        Ok(sent)
    }

    /// How many positions are waiting to be sent.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Storage`] when the queue cannot be read.
    pub async fn pending_progress_count(&self) -> Result<u32, BeamError> {
        let server_id = self.require_active()?;
        let queue = self.with_context(&server_id, |context| Arc::clone(&context.queue))?;
        Ok(u32::try_from(queue.len().await?).unwrap_or(u32::MAX))
    }

    // -- playback telemetry -----------------------------------------------

    /// Which kind of client this is, for every playback telemetry report.
    ///
    /// Set once at start-up, beside the device profile. A client that never
    /// sets it reports as [`TelemetryClientKind::Other`].
    pub fn set_playback_client_kind(&self, kind: TelemetryClientKind) {
        *self.client_kind.write().expect("client kind lock") = kind;
    }

    /// A playback started: its first frame is on screen.
    ///
    /// `audio_track_index` is the index into the source's `audio_tracks` of
    /// the track that is playing. Every `record_*` call answers how many events
    /// are now waiting, so the player can flush once a request's worth has
    /// built up; zero once the server has said telemetry is off.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `file_id` is not a UUID, and
    /// [`BeamError::NoActiveServer`] when no server is selected.
    pub fn record_playback_start(
        &self,
        file_id: String,
        audio_track_index: Option<u32>,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(Some(PlaybackEvent::Start {
            file_id,
            audio_track_index,
        }))
    }

    /// A playback failed to start.
    ///
    /// A failure after the first frame is not a start failure and is not
    /// reported. A [`StartFailureCause::Rejected`] for a policy exclusion is
    /// accepted and not counted.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`].
    pub fn record_playback_start_failure(
        &self,
        file_id: String,
        cause: StartFailureCause,
        audio_track_index: Option<u32>,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(PlaybackEvent::start_failure(
            file_id,
            cause,
            audio_track_index,
        ))
    }

    /// Playback stalled mid-stream for `duration_ms`.
    ///
    /// Only a stall after the first frame, and never the buffering a seek or a
    /// source switch causes: those are the player filling an empty buffer on
    /// purpose. A zero-length stall is not counted; one over ten minutes is
    /// counted as ten minutes.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`].
    pub fn record_playback_rebuffer(
        &self,
        file_id: String,
        duration_ms: u64,
    ) -> Result<u32, BeamError> {
        let file_id = parse_uuid("file_id", &file_id)?;
        self.record_telemetry(PlaybackEvent::rebuffer(file_id, duration_ms))
    }

    /// Playback moved from one source of a title to another.
    ///
    /// # Errors
    ///
    /// As [`Self::record_playback_start`], for either identifier.
    pub fn record_playback_source_switch(
        &self,
        from_file_id: String,
        to_file_id: String,
        trigger: SwitchTrigger,
    ) -> Result<u32, BeamError> {
        let from_file_id = parse_uuid("from_file_id", &from_file_id)?;
        let to_file_id = parse_uuid("to_file_id", &to_file_id)?;
        self.record_telemetry(PlaybackEvent::source_switch(
            from_file_id,
            to_file_id,
            trigger,
        ))
    }

    /// Report everything waiting, at most
    /// [`crate::telemetry::MAX_EVENTS_PER_REQUEST`] events to
    /// a request.
    ///
    /// Called on pause, on stop, when the app leaves the foreground, and once
    /// a `record_*` call says a request's worth is waiting. A 409 means the
    /// operator has telemetry off: the queue lets go of everything and stops
    /// for the rest of the session. A refused batch -- 422 above all -- is
    /// dropped, and its pointers logged, because the identical batch is
    /// refused identically. Anything transient stops the flush and keeps the
    /// batch for the next one.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// [`BeamError::SessionExpired`] when the server rejected the session
    /// part-way through -- which signs the device out, as it does for
    /// [`Self::flush_progress`]. Every other failure is an outcome, not an
    /// error: telemetry is never worth interrupting the viewer for.
    pub async fn flush_playback_telemetry(&self) -> Result<TelemetryFlush, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let telemetry = self.with_context(&server_id, |context| Arc::clone(&context.telemetry))?;
        let client_kind = *self.client_kind.read().expect("client kind lock");

        let mut outcome = TelemetryFlush::default();
        loop {
            let batch = telemetry.take_batch();
            if batch.is_empty() {
                break;
            }
            let size = u32::try_from(batch.len()).unwrap_or(u32::MAX);
            let body = batch_body(client_kind, &batch);
            let Err(failure) =
                TransportFailure::capture(client.report_playback_telemetry(&body)).await
            else {
                outcome.sent = outcome.sent.saturating_add(size);
                continue;
            };

            let mapped = self.fail(&server_id, &failure).await;
            match &mapped {
                BeamError::Server {
                    status: 409, code, ..
                } if code == TELEMETRY_DISABLED => {
                    telemetry.disable();
                    break;
                }
                refused if refused.is_permanent() => {
                    let pointers = failure
                        .problem
                        .as_ref()
                        .map(|problem| problem.pointers.join(", "))
                        .unwrap_or_default();
                    tracing::warn!(
                        server_id,
                        error = %refused,
                        pointers,
                        events = size,
                        "the server refused a playback telemetry batch; dropping it"
                    );
                    outcome.dropped = outcome.dropped.saturating_add(size);
                }
                transient => {
                    telemetry.restore(batch);
                    if matches!(transient, BeamError::SessionExpired) {
                        return Err(mapped);
                    }
                    break;
                }
            }
        }
        outcome.pending = telemetry.len();
        outcome.disabled = telemetry.is_disabled();
        Ok(outcome)
    }

    /// Playback telemetry for an inclusive range of UTC days, summed into the
    /// admin views.
    ///
    /// `from` and `to` are `YYYY-MM-DD`. Either may be omitted: the server
    /// then answers the thirty days ending today. Readable whether or not
    /// collection is on, so an operator can read what was kept after turning
    /// it off.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] for a date that does not parse, or a
    /// range the server refuses, and [`BeamError::Forbidden`] for a
    /// non-administrator.
    pub async fn admin_playback_telemetry(
        &self,
        from: Option<String>,
        to: Option<String>,
    ) -> Result<PlaybackTelemetrySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetPlaybackTelemetryParams {
            from: from
                .as_deref()
                .map(|day| parse_day("from", day))
                .transpose()?,
            to: to.as_deref().map(|day| parse_day("to", day)).transpose()?,
        };
        let response = self
            .send(&server_id, client.get_playback_telemetry(params))
            .await?;
        Ok(PlaybackTelemetrySummary::from_report(response.into_inner()))
    }

    // -- sessions ---------------------------------------------------------

    /// Every device signed in as this user.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
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
    /// Propagates transport and server failures.
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
    /// Propagates transport and server failures.
    pub async fn logout_everywhere(&self) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.logout_all()).await?;

        let effects = self.apply(&server_id, SessionEvent::LogoutRequested)?;
        self.perform(&server_id, &effects, None).await
    }

    // -- administration ---------------------------------------------------

    /// The admin dashboard snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_status(&self) -> Result<AdminStatus, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.get_admin_status()).await?;
        Ok(AdminStatus::from_generated(response.into_inner()))
    }

    /// One page of user accounts, oldest first: `first` accounts, after the
    /// page whose `end_cursor` is `after`, or from the start.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_users(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<AdminUserPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::ListAdminUsersParams {
            first: first.map(i64::from),
            after,
        };
        let response = self
            .send(&server_id, client.list_admin_users(params))
            .await?
            .into_inner();
        Ok(AdminUserPage {
            total: u64::try_from(response.total).unwrap_or(0),
            end_cursor: response.page_info.end_cursor,
            has_next_page: response.page_info.has_next_page,
            items: response
                .items
                .into_iter()
                .map(|user| AdminUser::from_generated(user, &record))
                .collect(),
        })
    }

    /// Block or unblock an account.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `user_id` is not a UUID, and
    /// [`BeamError::Forbidden`] for a non-administrator.
    pub async fn set_user_disabled(
        &self,
        user_id: String,
        disabled: bool,
    ) -> Result<(), BeamError> {
        let wire_user_id = parse_uuid("user_id", &user_id)?;
        let (server_id, client, _) = self.active_context()?;
        let body = crate::api::types::UpdateAdminUserRequest { disabled };
        self.send(&server_id, client.update_admin_user(wire_user_id, &body))
            .await?;
        Ok(())
    }

    /// One page of the operational log, newest first: `first` lines, after
    /// the page whose `end_cursor` is `after`, or from the start. `total`
    /// counts every line the server holds.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_logs(
        &self,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<AdminLogPage, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetAdminLogsParams {
            first: first.map(i64::from),
            after,
        };
        let response = self
            .send(&server_id, client.get_admin_logs(params))
            .await?
            .into_inner();
        Ok(AdminLogPage {
            total: u64::try_from(response.total).unwrap_or(0),
            end_cursor: response.page_info.end_cursor,
            has_next_page: response.page_info.has_next_page,
            items: response
                .items
                .into_iter()
                .map(AdminLogEntry::from_generated)
                .collect(),
        })
    }

    /// Recent server events, newest first.
    ///
    /// This polls `GET /v1/admin/events` rather than subscribing to
    /// `/v1/admin/events/stream`. Kynos now describes the streaming endpoint
    /// with OpenAPI 3.2's `itemSchema`, and spargen lowers it to
    /// `Client::stream_admin_events` returning
    /// `support::EventStream<types::AdminEvent>`. Nothing on the UniFFI
    /// surface consumes a stream yet -- UniFFI has no async-iterator type, so
    /// exposing it needs a callback-interface subscription rather than a return
    /// value -- so the feed still polls.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_events(&self, limit: Option<u32>) -> Result<Vec<AdminEvent>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetAdminEventsParams {
            first: limit.map(i64::from),
        };
        let response = self
            .send(&server_id, client.get_admin_events(params))
            .await?;
        // A snapshot, not a paged list: `items` is the most recent `limit`.
        Ok(response
            .into_inner()
            .items
            .into_iter()
            .map(AdminEvent::from_generated)
            .collect())
    }

    /// Create a library from a path on the server.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn create_library(
        &self,
        name: String,
        root_path: String,
    ) -> Result<LibrarySummary, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let body = crate::api::types::CreateLibraryRequest { name, root_path };
        let response = self.send(&server_id, client.create_library(&body)).await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// Delete a library and everything indexed into it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn delete_library(&self, library_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.delete_library(parse_uuid("library_id", &library_id)?),
        )
        .await?;
        Ok(())
    }

    /// Start a rescan of a library. Returns once the server has accepted it:
    /// the scan runs on the server afterwards, and its progress arrives on the
    /// admin event feed.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `library_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator, and
    /// [`BeamError::Server`] with status 409 while a scan of the library is
    /// already running.
    pub async fn scan_library(&self, library_id: String) -> Result<(), BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.scan_library(wire_library_id))
            .await?;
        Ok(())
    }

    /// The latest scan of a library since the server started, running or
    /// finished; `None` when it has not been scanned since.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::BadRequest`] when `library_id` is not a UUID,
    /// [`BeamError::Forbidden`] for a non-administrator, and
    /// [`BeamError::NotFound`] for a library the server does not know.
    pub async fn library_scan(&self, library_id: String) -> Result<Option<ScanJob>, BeamError> {
        let wire_library_id = parse_uuid("library_id", &library_id)?;
        let (server_id, client, _) = self.active_context()?;
        match self
            .send(&server_id, client.get_library_scan(wire_library_id))
            .await
        {
            Ok(response) => Ok(Some(ScanJob::from_generated(response.into_inner()))),
            // The server forgets scans when it restarts, and says so with its
            // own problem type: that is an answer -- not scanned since --
            // rather than a failure, unlike the library itself being unknown.
            Err(BeamError::NotFound { code, .. }) if code.ends_with(SCAN_NOT_FOUND) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Re-fetch metadata for one title.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn refresh_media_metadata(&self, media_id: String) -> Result<(), BeamError> {
        let wire_media_id = parse_uuid("media_id", &media_id)?;
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.refresh_media_metadata(wire_media_id, None),
        )
        .await?;
        Ok(())
    }

    /// The server's own health report.
    ///
    /// # Errors
    ///
    /// Propagates transport failures.
    pub async fn health(&self) -> Result<ServerHealth, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.get_health()).await?;
        Ok(ServerHealth::from_generated(response.into_inner()))
    }
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

/// A `YYYY-MM-DD` day, for a date query parameter.
fn parse_day(field: &str, value: &str) -> Result<crate::api::Date, BeamError> {
    time::Date::parse(value, &time::format_description::well_known::Iso8601::DATE)
        .map(crate::api::Date)
        .map_err(|_| BeamError::BadRequest {
            detail: format!("{field} is not a YYYY-MM-DD date"),
            code: ABOUT_BLANK.to_owned(),
        })
}

impl BeamClient {
    /// Build a server's context from scratch and put it in the registry.
    ///
    /// `carried` is the session state to keep across the rebuild, for the one
    /// caller that is rebuilding a server it already had. `None` derives it
    /// from the cookie, which is what a cold start and a newly added server
    /// want. Deliberately only the *state*: the queue is persisted under the
    /// server's own key and reloads itself, and the throttle and the metadata
    /// cache are operational rather than security-bearing, so rebuilding them
    /// costs a round trip and no correctness.
    fn install(
        &self,
        record: ServerRecord,
        cookie: Option<SecretString>,
        carried: Option<SessionState>,
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
        // nothing rebuilds the client for a session change any more, so the
        // connection pool and TLS session cache that Media3's neighbouring
        // range requests benefit from outlive everything but a trust change.
        let transport: Arc<dyn crate::api::HttpBackend> = Arc::new(ReqwestBackend::new(http));
        let cookie = Arc::new(SessionCookie::new(cookie));
        let client = Self::client_over(transport, &middleware, &record.base_url, &cookie)?;

        // A restored cookie is trusted until a request says otherwise, which
        // is what keeps a cold start off the network.
        let state = carried.unwrap_or(if cookie.is_registered() {
            SessionState::Expired
        } else {
            SessionState::LoggedOut
        });

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
                    // Removing an absent key succeeds, so this asks no
                    // question the caller would have to answer twice.
                    self.storage
                        .remove_secret(format!("session/{server_id}"))
                        .await?;
                    self.forget_abandoned(server_id);
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

    /// Queue `event` for the active server, where there is one to queue.
    ///
    /// `None` is an event the vocabulary does not count -- a policy
    /// exclusion, a zero-length stall, a switch to the same file -- and
    /// changes nothing.
    fn record_telemetry(&self, event: Option<PlaybackEvent>) -> Result<u32, BeamError> {
        let server_id = self.require_active()?;
        self.with_context(&server_id, |context| match event {
            Some(event) => context.telemetry.record(event),
            None => context.telemetry.len(),
        })
    }

    fn require_active(&self) -> Result<String, BeamError> {
        self.active
            .read()
            .expect("active lock")
            .clone()
            .ok_or(BeamError::NoActiveServer)
    }

    fn require_device_profile(&self) -> Result<DeviceProfile, BeamError> {
        self.device_profile
            .read()
            .expect("profile lock")
            .clone()
            .ok_or_else(|| BeamError::BadRequest {
                detail: "the device profile has not been set".to_owned(),
                // Refused here, so there is no server type to carry.
                code: ABOUT_BLANK.to_owned(),
            })
    }

    fn summary(&self, server_id: &str) -> Result<ServerSummary, BeamError> {
        self.list_servers()?
            .into_iter()
            .find(|summary| summary.id == server_id)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })
    }

    /// Turn a generated transport error into the core taxonomy, and drive the
    /// session machine when the server rejected our credential.
    ///
    /// `status` is the response's, where there was a response. Everything that
    /// carried one is classified from it -- so a 404 is a `NotFound` a viewer
    /// can be told about, and a 400 is not offered a retry. Only a failure that
    /// never reached a response is a `Network`.
    ///
    /// This took `&error.to_string()` and returned `Network { retryable: true }`
    /// for every one of them. `NotFound`, `Forbidden`, `BadRequest`, `Server`
    /// and `RateLimited` were unreachable from the server, `is_retryable` said
    /// "retry" for a 400, and the doc comments on the operations below promised
    /// variants the code could not produce (issue #123).
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
                // stands, and the credential is already off the client. Marked,
                // because the cookie the server rejected is still in the
                // keystore and a restore would otherwise send it again.
                tracing::warn!(server_id, %error, "could not delete the rejected session cookie");
                self.abandon_session(server_id);
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

/// Normalise a generated `MediaSource` into the core's own view, resolving its
/// relative URLs against the server that served it.
fn to_view(
    source: crate::api::types::MediaSource,
    record: &ServerRecord,
) -> Result<MediaSourceView, BeamError> {
    // JSON Schema has no unsigned integer type, so kynos's `uint32`/`uint64`
    // formats reach the generated client as `i64`. Narrowing here rather than
    // widening the core's own types keeps "a size cannot be negative" true in
    // the one place that reasons about sizes; a nonsensical negative is
    // treated as absent rather than wrapping into an enormous positive.
    //
    // The view describes one picture: the source's first video track, which
    // the server lists by stream index. The server lists sources primary
    // first, and this keeps that order.
    let video = source.video_tracks.into_iter().next();
    let parts = source
        .parts
        .into_iter()
        .map(|part| {
            Ok(crate::capability::select::SourcePartView {
                file_id: part.file_id.to_string(),
                part_number: part
                    .part_number
                    .and_then(|number| u32::try_from(number).ok()),
                size_bytes: u64::try_from(part.size_bytes).unwrap_or(0),
                duration_secs: part.duration_secs,
                stream_url: record.absolute_url(&part.stream_url)?,
                subtitle_tracks: subtitle_views(part.subtitle_tracks, record)?,
            })
        })
        .collect::<Result<Vec<_>, BeamError>>()?;
    Ok(MediaSourceView {
        file_id: source.file_id.to_string(),
        size_bytes: u64::try_from(source.size_bytes).unwrap_or(0),
        duration_secs: source.duration_secs,
        container: source.container_format,
        mime_type: source.mime_type,
        video_codec: video.as_ref().map(|v| v.codec.clone()),
        width: video.as_ref().and_then(|v| u32::try_from(v.width).ok()),
        height: video.as_ref().and_then(|v| u32::try_from(v.height).ok()),
        bit_rate: video
            .as_ref()
            .and_then(|v| v.bit_rate)
            .and_then(|rate| u64::try_from(rate).ok()),
        hdr_format: video.as_ref().and_then(|v| v.hdr_format.clone()),
        audio_tracks: source
            .audio_tracks
            .into_iter()
            .map(|track| crate::capability::select::AudioTrackView {
                codec: track.codec,
                language: track.language,
                channels: u16::try_from(track.channels).unwrap_or(0),
                is_default: track.is_default,
            })
            .collect(),
        stream_url: record.absolute_url(&source.stream_url)?,
        download_url: record.absolute_url(&source.download_url)?,
        parts,
        disc_structure: source.disc_structure.map(|structure| match structure {
            crate::api::types::MediaSourcediscStructure::Dvd => {
                crate::capability::select::DiscStructure::Dvd
            }
            crate::api::types::MediaSourcediscStructure::BluRay => {
                crate::capability::select::DiscStructure::BluRay
            }
        }),
        subtitle_tracks: subtitle_views(source.subtitle_tracks, record)?,
    })
}

/// Normalise a file's subtitle tracks, resolving a sidecar's URLs against the
/// server that served them.
fn subtitle_views(
    tracks: Vec<crate::api::types::SubtitleTrack>,
    record: &ServerRecord,
) -> Result<Vec<crate::capability::select::SubtitleTrackView>, BeamError> {
    use crate::capability::select::{SubtitleOrigin, SubtitleTrackView};
    tracks
        .into_iter()
        .map(|track| {
            let crate::api::types::SubtitleTrack {
                origin,
                index,
                sidecar_id,
                codec,
                language,
                title,
                is_default,
                is_forced,
                is_hearing_impaired,
                is_text,
                url,
                webvtt_url,
            } = track;
            Ok(SubtitleTrackView {
                origin: match origin {
                    crate::api::types::SubtitleOrigin::Embedded => SubtitleOrigin::Embedded,
                    crate::api::types::SubtitleOrigin::Sidecar => SubtitleOrigin::Sidecar,
                },
                index: index.and_then(|value| u32::try_from(value).ok()),
                sidecar_id: sidecar_id.map(|id| id.to_string()),
                codec,
                language,
                title,
                is_default,
                is_forced,
                is_hearing_impaired,
                is_text,
                url: url.map(|path| record.absolute_url(&path)).transpose()?,
                webvtt_url: webvtt_url
                    .map(|path| record.absolute_url(&path))
                    .transpose()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::kv::{FailureMode, InMemoryKeyValueStore};
    use crate::transport::CannedBackend;

    /// Everything here exercises the façade without a network. That is most of
    /// it: the registry, the session state machine, trust decisions, and the
    /// configuration handed to the platform player are all local, and they are
    /// the parts a mistake in would be invisible until a device ran them.
    fn client() -> Arc<BeamClient> {
        BeamClient::new(Arc::new(InMemoryKeyValueStore::new()))
    }

    async fn client_with_server() -> (Arc<BeamClient>, String) {
        let client = client();
        let summary = client
            .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
            .await
            .expect("adding a server is offline");
        let id = summary.id.clone();
        (client, id)
    }

    #[tokio::test]
    async fn a_new_client_knows_no_servers() {
        let client = client();
        assert!(client.list_servers().expect("listable").is_empty());
        assert!(matches!(
            client.playback_config("f1".to_owned()),
            Err(BeamError::NoActiveServer)
        ));
    }

    #[tokio::test]
    async fn adding_a_server_makes_it_active() {
        let (client, id) = client_with_server().await;

        let servers = client.list_servers().expect("listable");
        assert_eq!(servers.len(), 1);
        assert!(servers[0].is_active);
        assert_eq!(servers[0].id, id);
        assert_eq!(servers[0].display_name, "Home");
    }

    #[tokio::test]
    async fn adding_the_same_server_twice_is_idempotent() {
        // Typing the same address again means "go there", not "fail" -- and
        // certainly not "register it twice".
        let (client, id) = client_with_server().await;

        let again = client
            .add_server("https://beam.test/".to_owned(), None)
            .await
            .expect("idempotent");

        assert_eq!(again.id, id);
        assert_eq!(client.list_servers().expect("listable").len(), 1);
        assert_eq!(
            again.display_name, "Home",
            "re-adding must not discard the name the user gave it"
        );
    }

    #[tokio::test]
    async fn an_unusable_address_is_rejected_before_anything_is_stored() {
        let client = client();

        assert!(matches!(
            client.add_server("not a url".to_owned(), None).await,
            Err(BeamError::InvalidServerUrl { .. })
        ));
        assert!(client.list_servers().expect("listable").is_empty());
    }

    #[tokio::test]
    async fn a_registry_survives_a_restart() {
        // The whole point of persisting it: a cold start must not send the
        // viewer back to the address field.
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        first
            .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
            .await
            .expect("added");

        let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        let restored = second.restore().await.expect("restored");

        assert_eq!(restored.len(), 1);
        assert!(restored[0].is_active, "the active choice must survive too");
    }

    #[tokio::test]
    async fn a_restored_session_is_expired_until_a_request_confirms_it() {
        // Optimistic restore keeps app start off the network, but the cookie
        // has not been checked, so claiming Authenticated would be a lie.
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let summary = first
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added");
        storage
            .put_secret(format!("session/{}", summary.id), "opaque".to_owned())
            .await
            .expect("stored");

        let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        second.restore().await.expect("restored");

        assert!(matches!(
            second.session_state(summary.id).expect("known"),
            SessionState::Expired
        ));
    }

    #[tokio::test]
    async fn a_server_with_no_stored_cookie_restores_as_logged_out() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let summary = first
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added");

        let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        second.restore().await.expect("restored");

        assert!(matches!(
            second.session_state(summary.id).expect("known"),
            SessionState::LoggedOut
        ));
    }

    #[tokio::test]
    async fn removing_a_server_forgets_it_and_its_secret() {
        let (client, id) = client_with_server().await;

        client.remove_server(id.clone()).await.expect("removed");

        assert!(client.list_servers().expect("listable").is_empty());
        assert!(matches!(
            client.session_state(id),
            Err(BeamError::UnknownServer { .. })
        ));
    }

    #[tokio::test]
    async fn selecting_an_unknown_server_is_an_error() {
        let client = client();
        assert!(matches!(
            client.select_server("nope".to_owned()).await,
            Err(BeamError::UnknownServer { .. })
        ));
    }

    #[tokio::test]
    async fn the_login_url_is_built_from_the_server_origin() {
        let (client, id) = client_with_server().await;

        let url = client.login_url(id).expect("built");

        assert!(url.starts_with("https://beam.test/v1/auth/login"));
        assert!(
            url.contains("redirect="),
            "the server only accepts a same-origin relative redirect"
        );
    }

    #[tokio::test]
    async fn playback_needs_a_session_before_it_will_hand_over_a_url() {
        // Returning a URL with no cookie would produce a request the server
        // answers with 401, surfacing to the viewer as a broken file.
        let (client, _) = client_with_server().await;

        assert!(matches!(
            client.playback_config("file-1".to_owned()),
            Err(BeamError::Unauthenticated)
        ));
        assert!(matches!(
            client.server_http_config(),
            Err(BeamError::Unauthenticated)
        ));
    }

    #[tokio::test]
    async fn a_playback_config_carries_the_cookie_and_the_stream_url() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let summary = client
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added");
        storage
            .put_secret(format!("session/{}", summary.id), "opaque-value".to_owned())
            .await
            .expect("stored");
        let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        client.restore().await.expect("restored");

        let config = client
            .playback_config("file-1".to_owned())
            .expect("configured");

        assert_eq!(config.url, "https://beam.test/v1/files/file-1/stream");
        assert_eq!(
            config.headers.get("Cookie").map(String::as_str),
            Some("beam_session=opaque-value"),
            "Media3 fetches the bytes itself, so the credential has to travel with it"
        );
        assert_eq!(config.pinned_host, "beam.test");
    }

    #[tokio::test]
    async fn trusting_a_certificate_records_it_and_survives_a_restart() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let summary = client
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added");

        client
            .trust_certificate(summary.id.clone(), "AA:BB:CC".to_owned())
            .await
            .expect("trusted");

        assert_eq!(
            client
                .trusted_certificates(summary.id.clone())
                .expect("known"),
            vec!["AA:BB:CC".to_owned()]
        );

        // A trust decision the user made once must not be forgotten on restart,
        // or they would be asked again on every cold start.
        let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        restarted.restore().await.expect("restored");
        assert_eq!(
            restarted.trusted_certificates(summary.id).expect("known"),
            vec!["AA:BB:CC".to_owned()]
        );
    }

    #[tokio::test]
    async fn trusting_the_same_certificate_twice_does_not_duplicate_it() {
        let (client, id) = client_with_server().await;

        client
            .trust_certificate(id.clone(), "AA:BB".to_owned())
            .await
            .expect("trusted");
        client
            .trust_certificate(id.clone(), "AA:BB".to_owned())
            .await
            .expect("trusted again");

        assert_eq!(client.trusted_certificates(id).expect("known").len(), 1);
    }

    #[tokio::test]
    async fn forgetting_certificates_withdraws_every_one() {
        let (client, id) = client_with_server().await;
        client
            .trust_certificate(id.clone(), "AA:BB".to_owned())
            .await
            .expect("trusted");

        client
            .forget_certificates(id.clone())
            .await
            .expect("forgotten");

        assert!(client.trusted_certificates(id).expect("known").is_empty());
    }

    #[tokio::test]
    async fn trusting_a_certificate_for_an_unknown_server_is_an_error() {
        let client = client();
        assert!(matches!(
            client
                .trust_certificate("nope".to_owned(), "AA:BB".to_owned())
                .await,
            Err(BeamError::UnknownServer { .. })
        ));
    }

    #[tokio::test]
    async fn logging_out_clears_the_session_but_keeps_the_server() {
        // Signing out is not forgetting the address; the viewer will sign back
        // in to the same place.
        let (client, id) = client_with_server().await;

        client.logout(id.clone()).await.expect("logged out");

        assert!(matches!(
            client.session_state(id).expect("still known"),
            SessionState::LoggedOut
        ));
        assert_eq!(client.list_servers().expect("listable").len(), 1);
    }

    /// The title the transport tests ask about; what it names never matters.
    const MEDIA_ID: &str = "00000000-0000-0000-0000-000000000007";

    /// What `GET /v1/me` answers for the signed-in user in these tests.
    const ME: &str =
        r#"{"id":"00000000-0000-0000-0000-0000000000a1","display_name":"Ada","is_admin":false}"#;

    /// What `GET /v1/media/{id}/sources` answers for a title with no file.
    const NO_SOURCES: &str = r#"{"items":[],"page_info":{"has_next_page":false,"has_previous_page":false,"start_cursor":null,"end_cursor":null}}"#;

    /// What `GET /v1/media/{id}` answers for [`MEDIA_ID`].
    const MOVIE_DETAIL: &str = r#"{"Movie":{"id":"00000000-0000-0000-0000-000000000007","title":{"original":"Le Samourai"},"genres":[],"user_state":{"played":false,"position_secs":0.0,"play_count":0}}}"#;

    /// The same movie, once the viewer has marked it watched.
    const WATCHED_MOVIE_DETAIL: &str = r#"{"Movie":{"id":"00000000-0000-0000-0000-000000000007","title":{"original":"Le Samourai"},"genres":[],"user_state":{"played":true,"position_secs":0.0,"play_count":1}}}"#;

    /// Marking a title watched and reading it back shows it watched: the mark
    /// is a `PUT` on the title's `watched`, and the read after it reaches the
    /// server rather than a copy taken before the mark (issue #188).
    #[tokio::test]
    async fn a_watched_mark_shows_on_the_next_detail_read() {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering_in_turn(vec![
            (200, "application/json", MOVIE_DETAIL),
            (204, "application/json", ""),
            (200, "application/json", WATCHED_MOVIE_DETAIL),
        ]));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let before = client
            .media_detail(MEDIA_ID.to_owned())
            .await
            .expect("the canned detail is served");
        client
            .mark_watched(MEDIA_ID.to_owned())
            .await
            .expect("the canned 204 accepts the mark");
        let after = client
            .media_detail(MEDIA_ID.to_owned())
            .await
            .expect("the canned detail is served again");

        assert!(!before.summary().played);
        assert!(after.summary().played, "the read after the mark is fresh");
        let recorded = backend.recorded();
        assert_eq!(recorded[1].method, reqwest::Method::PUT);
        assert_eq!(
            recorded[1].url.path(),
            format!("/v1/media/{MEDIA_ID}/watched")
        );
        assert_eq!(recorded[2].method, reqwest::Method::GET);
    }

    #[tokio::test]
    async fn an_unwatched_mark_is_a_delete_of_the_titles_watched_state() {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(204, "application/json", ""));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        client
            .mark_unwatched(MEDIA_ID.to_owned())
            .await
            .expect("the canned 204 accepts the mark");

        let recorded = backend.recorded();
        assert_eq!(recorded[0].method, reqwest::Method::DELETE);
        assert_eq!(
            recorded[0].url.path(),
            format!("/v1/media/{MEDIA_ID}/watched")
        );
        assert!(matches!(
            client.mark_watched("not-a-uuid".to_owned()).await,
            Err(BeamError::BadRequest { .. })
        ));
    }

    const SHOW_ID: &str = "5a5a5a5a-0000-4000-8000-000000000001";
    const CURRENT_EPISODE: &str = "e0000001-0000-4000-8000-000000000001";
    const NEXT_EPISODE: &str = "e0000005-0000-4000-8000-000000000005";

    /// `GET /v1/episodes/{id}` for an episode of `show` whose next-up is
    /// `next` (JSON `null` for none).
    fn episode_detail(episode: &str, show: &str, next: &str) -> String {
        format!(
            r#"{{"episode":{{"id":"{episode}","episode_number":4,"title":"Return","source_count":1,
                "user_state":{{"played":false,"position_secs":0.0,"play_count":0}},
                "file_id":"f4f4f4f4-0000-4000-8000-000000000004"}},
               "season_id":"5e5e5e5e-0000-4000-8000-000000000001","season_number":2,
               "show":{{"id":"{show}","title":{{"original":"Dark"}}}},
               "next_episode_id":{next}}}"#
        )
    }

    /// A signed-in server answering each request with the next of `answers`.
    async fn client_answering_in_turn(
        answers: Vec<(u16, &'static str, &'static str)>,
    ) -> (Arc<BeamClient>, Arc<CannedBackend>) {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering_in_turn(answers));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        (client, backend)
    }

    /// Auto-advance follows the server's `next_episode_id`, and returns the
    /// next episode as the server describes it.
    #[tokio::test]
    async fn up_next_is_the_episode_the_server_names_next() {
        let current: &'static str = Box::leak(
            episode_detail(CURRENT_EPISODE, SHOW_ID, &format!("\"{NEXT_EPISODE}\""))
                .into_boxed_str(),
        );
        let next: &'static str =
            Box::leak(episode_detail(NEXT_EPISODE, SHOW_ID, "null").into_boxed_str());
        let (client, backend) = client_answering_in_turn(vec![
            (200, "application/json", current),
            (200, "application/json", next),
        ])
        .await;

        let episode = client
            .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
            .await
            .expect("both episodes decode")
            .expect("the server names a next episode");

        assert_eq!(episode.id, NEXT_EPISODE);
        assert_eq!(
            episode.file_id.as_deref(),
            Some("f4f4f4f4-0000-4000-8000-000000000004")
        );
        let paths: Vec<String> = backend
            .recorded()
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(
            paths,
            vec![
                format!("/v1/episodes/{CURRENT_EPISODE}"),
                format!("/v1/episodes/{NEXT_EPISODE}"),
            ]
        );
    }

    #[tokio::test]
    async fn there_is_no_up_next_after_the_last_episode() {
        let last: &'static str =
            Box::leak(episode_detail(CURRENT_EPISODE, SHOW_ID, "null").into_boxed_str());
        let (client, backend) =
            client_answering_in_turn(vec![(200, "application/json", last)]).await;

        let episode = client
            .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
            .await
            .expect("the episode decodes");

        assert_eq!(episode, None);
        assert_eq!(backend.recorded().len(), 1, "nothing to look up next");
    }

    #[tokio::test]
    async fn up_next_never_crosses_into_another_show() {
        let elsewhere: &'static str = Box::leak(
            episode_detail(
                CURRENT_EPISODE,
                "0f0f0f0f-0000-4000-8000-00000000000f",
                &format!("\"{NEXT_EPISODE}\""),
            )
            .into_boxed_str(),
        );
        let (client, backend) =
            client_answering_in_turn(vec![(200, "application/json", elsewhere)]).await;

        let episode = client
            .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
            .await
            .expect("the episode decodes");

        assert_eq!(episode, None);
        assert_eq!(backend.recorded().len(), 1);
    }

    /// `GET /v1/admin/libraries/{id}/scan` for a scan part-way through.
    const RUNNING_SCAN: &str = r#"{"id":"5c000000-0000-4000-8000-000000000001",
        "library_id":"00000000-0000-0000-0000-00000000000a","trigger":"manual",
        "state":"running","queued_at":"2026-01-01T00:00:00Z",
        "started_at":"2026-01-01T00:00:01Z","finished_at":null,
        "progress":{"total_count":120,"processed_count":30,"added_count":12,
                    "changed_count":3,"unchanged_count":15,"deferred_count":0,
                    "failed_count":1,"marked_missing_count":2,"restored_count":0,
                    "relinked_count":0,"purged_count":0},
        "failure":null}"#;
    const LIBRARY_ID: &str = "00000000-0000-0000-0000-00000000000a";

    #[tokio::test]
    async fn a_running_scan_reports_its_progress() {
        let (client, backend) =
            client_answering_in_turn(vec![(200, "application/json", RUNNING_SCAN)]).await;

        let job = client
            .library_scan(LIBRARY_ID.to_owned())
            .await
            .expect("the scan decodes")
            .expect("the library has a scan");

        assert_eq!(job.state, crate::catalog::ScanJobState::Running);
        assert_eq!(job.library_id, LIBRARY_ID);
        assert_eq!((job.processed_count, job.total_count), (30, Some(120)));
        assert_eq!(
            (
                job.added_count,
                job.changed_count,
                job.failed_count,
                job.missing_count
            ),
            (12, 3, 1, 2)
        );
        assert_eq!(job.finished_at_unix, None);
        assert_eq!(
            backend.recorded()[0].url.path(),
            format!("/v1/admin/libraries/{LIBRARY_ID}/scan")
        );
    }

    /// The server forgets scans when it restarts: that is "not scanned
    /// since", an answer, while an unknown library is still a failure.
    #[tokio::test]
    async fn no_scan_since_the_server_started_is_none_and_an_unknown_library_is_an_error() {
        let (client, _) = client_answering_in_turn(vec![
            (
                404,
                "application/problem+json",
                r#"{"type":"https://beam.justinchung.net/reference/errors/#scan-not-found","status":404,"title":"No scan of this library since the server started"}"#,
            ),
            (
                404,
                "application/problem+json",
                r#"{"type":"https://beam.justinchung.net/reference/errors/#library-not-found","status":404,"title":"Library not found"}"#,
            ),
        ])
        .await;

        assert_eq!(
            client
                .library_scan(LIBRARY_ID.to_owned())
                .await
                .expect("not scanned is an answer"),
            None
        );
        assert!(matches!(
            client.library_scan(LIBRARY_ID.to_owned()).await,
            Err(BeamError::NotFound { .. })
        ));
    }

    /// A library file, as `GET /v1/libraries/{id}/files` lists one.
    macro_rules! library_file {
        ($id:literal, $path:literal) => {
            concat!(
                r#"{"id":""#,
                $id,
                r#"","library_id":"00000000-0000-0000-0000-00000000000a","path":""#,
                $path,
                r#"","size_bytes":1,"hash":"1","status":"known","content_type":"movie","scanned_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#
            )
        };
    }

    /// The server pages a library's files; the core walks every page, each
    /// request after the first carrying the previous page's `end_cursor`, and
    /// returns the whole list in order.
    #[tokio::test]
    async fn a_library_s_files_are_walked_page_by_page() {
        let (client, id, _) = signed_in_client().await;
        let first_page: &'static str = concat!(
            r#"{"items":["#,
            library_file!("00000000-0000-0000-0000-000000000001", "a.mkv"),
            r#"],"page_info":{"has_next_page":true,"has_previous_page":false,"start_cursor":"c1","end_cursor":"c1"}}"#
        );
        let last_page: &'static str = concat!(
            r#"{"items":["#,
            library_file!("00000000-0000-0000-0000-000000000002", "b.mkv"),
            r#"],"page_info":{"has_next_page":false,"has_previous_page":true,"start_cursor":"c2","end_cursor":"c2"}}"#
        );
        let backend = Arc::new(CannedBackend::answering_in_turn(vec![
            (200, "application/json", first_page),
            (200, "application/json", last_page),
        ]));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let files = client
            .library_files("00000000-0000-0000-0000-00000000000a".to_owned())
            .await
            .expect("both pages decode");

        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["a.mkv", "b.mkv"]);
        let queries: Vec<String> = backend
            .recorded()
            .iter()
            .map(|request| request.url.query().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(queries.len(), 2, "one request per page: {queries:?}");
        assert!(!queries[0].contains("after="), "{}", queries[0]);
        assert!(queries[1].contains("after=c1"), "{}", queries[1]);
        assert!(
            queries.iter().all(|query| query.contains("first=500")),
            "{queries:?}"
        );
    }

    /// A page of the log carries the server's cursor and total through, and
    /// the next request sends the cursor it was handed.
    #[tokio::test]
    async fn a_page_of_the_log_carries_its_cursor_and_total() {
        let (client, id, _) = signed_in_client().await;
        let page: &'static str = r#"{"items":[{"id":"00000000-0000-0000-0000-000000000003","level":"info","category":"system","message":"started","created_at":"2026-01-01T00:00:00Z"}],"page_info":{"has_next_page":true,"has_previous_page":false,"start_cursor":"l0","end_cursor":"l0"},"total":7}"#;
        let backend = Arc::new(CannedBackend::answering(200, "application/json", page));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let logs = client
            .admin_logs(Some(1), Some("l-1".to_owned()))
            .await
            .expect("the page decodes");

        assert_eq!(logs.items.len(), 1);
        assert_eq!(logs.total, 7);
        assert_eq!(logs.end_cursor.as_deref(), Some("l0"));
        assert!(logs.has_next_page);
        let query = backend.recorded()[0]
            .url
            .query()
            .unwrap_or_default()
            .to_owned();
        assert!(
            query.contains("first=1") && query.contains("after=l-1"),
            "{query}"
        );
    }

    /// Sign in to a registered server against a canned `/v1/me`, through the
    /// production path: `complete_login` registers the cookie with the client
    /// and verifies it with the one secured request the flow makes.
    ///
    /// Returns the backend too, so a test can read what reached the wire.
    async fn signed_in_client() -> (Arc<BeamClient>, String, Arc<CannedBackend>) {
        signed_in_client_over(Arc::new(InMemoryKeyValueStore::new())).await
    }

    /// `signed_in_client`, over a storage the test keeps a handle to.
    async fn signed_in_client_over(
        storage: Arc<InMemoryKeyValueStore>,
    ) -> (Arc<BeamClient>, String, Arc<CannedBackend>) {
        let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        let id = client
            .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
            .await
            .expect("adding a server is offline")
            .id;
        let backend = Arc::new(CannedBackend::answering(200, "application/json", ME));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        client
            .complete_login(id.clone(), "opaque-value".to_owned())
            .await
            .expect("the canned /v1/me confirms the cookie");
        (client, id, backend)
    }

    /// The `Cookie` header values one recorded request carried.
    fn cookies_of(request: &crate::transport::RecordedRequest) -> Vec<&str> {
        request
            .headers
            .get_all(reqwest::header::COOKIE)
            .iter()
            .map(|value| value.to_str().expect("a cookie header is ASCII"))
            .collect()
    }

    /// A signed-in server that answers every further request with one canned
    /// response.
    ///
    /// Signed in, because every operation these tests make is one the spec
    /// secures with `BeamSession`, and the generated client refuses to build
    /// such a request without a registered credential -- so a logged-out
    /// call would never reach the canned answer at all.
    async fn client_answering(status: u16, body: &'static str) -> (Arc<BeamClient>, String) {
        let (client, id, _) = signed_in_client().await;
        client
            .use_transport(
                &id,
                Arc::new(CannedBackend::answering(
                    status,
                    "application/problem+json",
                    body,
                )),
            )
            .expect("the server is registered");
        (client, id)
    }

    /// The whole reason the credential lives on the client: a secured call
    /// from a signed-in device has to reach the server carrying the cookie --
    /// once, as the spec's `BeamSession` scheme spells it.
    #[tokio::test]
    async fn a_signed_in_call_carries_the_session_cookie_exactly_once() {
        let (client, _, backend) = signed_in_client().await;

        // The canned body is a `CurrentUser`, so this decodes to nothing
        // useful; what matters is that the request left at all, and how.
        let _ = client.media_sources(MEDIA_ID.to_owned()).await;

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), 2, "the /v1/me of the sign-in, then this");
        let sources = &recorded[1];
        assert_eq!(sources.method, reqwest::Method::GET);
        assert_eq!(sources.url.path(), format!("/v1/media/{MEDIA_ID}/sources"));
        assert_eq!(cookies_of(sources), vec!["beam_session=opaque-value"]);
        assert_eq!(
            cookies_of(&recorded[0]),
            vec!["beam_session=opaque-value"],
            "the sign-in's own /v1/me is a secured call too"
        );
    }

    /// Signing out revokes the session on the server, not only on the device:
    /// the `POST /v1/logout` carries the cookie it is ending, read before the
    /// transition withdraws it. Without it the server answers 204 and leaves
    /// the session valid until it expires.
    #[tokio::test]
    async fn logout_sends_the_session_it_is_ending() {
        let (client, id, backend) = signed_in_client().await;
        let sent_before = backend.recorded().len();

        client.logout(id.clone()).await.expect("logged out");

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), sent_before + 1, "exactly the revoke");
        let revoke = recorded.last().expect("the revoke was sent");
        assert_eq!(revoke.method, reqwest::Method::POST);
        assert_eq!(revoke.url.path(), "/v1/logout");
        assert_eq!(cookies_of(revoke), vec!["beam_session=opaque-value"]);
        assert_eq!(
            client.session_state(id).expect("registered"),
            SessionState::LoggedOut,
            "and the device is signed out"
        );
    }

    /// Signing out withdraws the credential, and the generated client then
    /// refuses the request before it is sent: nothing reaches the transport,
    /// and what the caller gets is "sign in" -- not a malformed request, and
    /// not something permanent, because signing in cures it.
    #[tokio::test]
    async fn after_logout_a_secured_call_never_reaches_the_transport() {
        let (client, id, backend) = signed_in_client().await;
        client.logout(id).await.expect("logged out");
        let sent_before = backend.recorded().len();

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("no credential, no request");

        assert_eq!(error, BeamError::Unauthenticated);
        assert!(
            !error.is_permanent(),
            "a resume point must wait for the sign-in"
        );
        assert_eq!(
            backend.recorded().len(),
            sent_before,
            "the refusal happens at construction, not on the wire"
        );
    }

    /// The twin of the logout case: once the server has rejected the session,
    /// the next secured call is refused at construction too, and it says
    /// "your session expired" rather than "sign in" -- the state the 401 left
    /// behind is what tells the two apart.
    #[tokio::test]
    async fn after_an_expiry_a_secured_call_is_reported_as_expired_not_malformed() {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        let _ = client.media_sources(MEDIA_ID.to_owned()).await;
        let sent_before = backend.recorded().len();

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("no credential, no request");

        assert_eq!(error, BeamError::SessionExpired);
        assert!(!error.is_permanent(), "{error:?}");
        assert_eq!(backend.recorded().len(), sent_before);
    }

    /// The server's one track model (issue #189) reaches the platform as the
    /// view it already knew: the picture from the first video track, every
    /// audio track, the codecs as the server named them, the sources in the
    /// server's order, and the URLs resolved against the server.
    #[tokio::test]
    async fn sources_become_views_of_their_first_video_track() {
        const SOURCES: &str = r#"{"items":[
            {"file_id":"f1f1f1f1-0000-4000-8000-000000000001","is_primary":true,
             "edition":null,"episode_span":null,
             "parts":[
                {"file_id":"f1f1f1f1-0000-4000-8000-000000000001","part_number":null,
                 "size_bytes":4000,"duration_secs":6300.0,"subtitle_tracks":[],
                 "stream_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream",
                 "download_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/download"}],
             "size_bytes":4000,
             "mime_type":"video/x-matroska","container_format":"matroska",
             "duration_secs":6300.0,
             "video_tracks":[
                {"index":0,"codec":"hevc","width":3840,"height":2160,
                 "frame_rate":null,"bit_rate":20000000,"hdr_format":"HDR10"},
                {"index":3,"codec":"mjpeg","width":600,"height":900,
                 "frame_rate":null,"bit_rate":null,"hdr_format":null}],
             "audio_tracks":[
                {"index":1,"codec":"truehd","language":"eng","title":null,
                 "channels":8,"channel_layout":"7.1","sample_rate":48000,
                 "bit_rate":null,"is_default":true,"is_forced":false},
                {"index":2,"codec":"eac3","language":"fre","title":null,
                 "channels":6,"channel_layout":null,"sample_rate":null,
                 "bit_rate":null,"is_default":false,"is_forced":false}],
             "subtitle_tracks":[
                {"origin":"sidecar","index":null,
                 "sidecar_id":"5c5c5c5c-0000-4000-8000-000000000001",
                 "codec":"subrip","language":"eng","title":null,
                 "is_default":false,"is_forced":false,"is_hearing_impaired":true,
                 "is_text":true,
                 "url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/subtitles/5c5c5c5c-0000-4000-8000-000000000001",
                 "webvtt_url":null}],
             "stream_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream",
             "download_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/download"},
            {"file_id":"f2f2f2f2-0000-4000-8000-000000000002","is_primary":false,
             "edition":"Director's Cut","episode_span":null,
             "parts":[
                {"file_id":"f2f2f2f2-0000-4000-8000-000000000002","part_number":null,
                 "size_bytes":1000,"duration_secs":null,"subtitle_tracks":[],
                 "stream_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/stream",
                 "download_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/download"}],
             "size_bytes":1000,
             "mime_type":null,"container_format":null,"duration_secs":null,
             "video_tracks":[],"audio_tracks":[],"subtitle_tracks":[],
             "stream_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/stream",
             "download_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/download"}
        ],
        "page_info":{"has_next_page":false,"has_previous_page":false,
                     "start_cursor":null,"end_cursor":null}}"#;
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(200, "application/json", SOURCES));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let views = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect("the sources decode");

        assert_eq!(views.len(), 2);
        let primary = &views[0];
        assert_eq!(primary.file_id, "f1f1f1f1-0000-4000-8000-000000000001");
        assert_eq!(primary.video_codec.as_deref(), Some("hevc"));
        assert_eq!((primary.width, primary.height), (Some(3840), Some(2160)));
        assert_eq!(primary.bit_rate, Some(20_000_000));
        assert_eq!(primary.hdr_format.as_deref(), Some("HDR10"));
        let audio: Vec<(&str, u16, bool)> = primary
            .audio_tracks
            .iter()
            .map(|a| (a.codec.as_str(), a.channels, a.is_default))
            .collect();
        assert_eq!(audio, vec![("truehd", 8, true), ("eac3", 6, false)]);
        assert_eq!(
            primary.stream_url,
            "https://beam.test/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream"
        );

        let other = &views[1];
        assert_eq!(other.file_id, "f2f2f2f2-0000-4000-8000-000000000002");
        assert_eq!(other.video_codec, None, "no video track, no picture");
        assert_eq!((other.width, other.height), (None, None));
    }

    /// A multi-part source (issue #233) carries every part in the server's
    /// order with its own absolute stream URL and its own sidecars, a disc rip
    /// (issue #234) its structure, and a sidecar subtitle (issue #189) the
    /// URLs the player fetches it from -- resolved, so the player needs no
    /// base URL of its own.
    #[tokio::test]
    async fn a_multi_part_disc_source_carries_its_parts_structure_and_sidecars() {
        const SOURCES: &str = r#"{"items":[
            {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","is_primary":true,
             "edition":null,"episode_span":null,"disc_structure":"blu_ray",
             "parts":[
                {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","part_number":1,
                 "size_bytes":3000,"duration_secs":3000.0,"subtitle_tracks":[],
                 "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
                 "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"},
                {"file_id":"c2c2c2c2-0000-4000-8000-000000000002","part_number":2,
                 "size_bytes":2000,"duration_secs":2800.0,
                 "subtitle_tracks":[
                    {"origin":"sidecar","index":null,
                     "sidecar_id":"5d5d5d5d-0000-4000-8000-000000000002",
                     "codec":"subrip","language":"spa","title":"Forced",
                     "is_default":false,"is_forced":true,"is_hearing_impaired":false,
                     "is_text":true,
                     "url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002",
                     "webvtt_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002/webvtt"}],
                 "stream_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream",
                 "download_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/download"}],
             "size_bytes":5000,
             "mime_type":null,"container_format":"mpegts","duration_secs":5800.0,
             "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                              "frame_rate":null,"bit_rate":null,"hdr_format":null}],
             "audio_tracks":[],
             "subtitle_tracks":[
                {"origin":"embedded","index":3,"sidecar_id":null,
                 "codec":"hdmv_pgs_subtitle","language":"eng","title":null,
                 "is_default":true,"is_forced":false,"is_hearing_impaired":false,
                 "is_text":false,"url":null,"webvtt_url":null}],
             "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
             "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"}
        ],
        "page_info":{"has_next_page":false,"has_previous_page":false,
                     "start_cursor":null,"end_cursor":null}}"#;
        let client = client_answering_sources(SOURCES).await;

        let views = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect("the sources decode");
        let source = &views[0];

        assert_eq!(
            source.disc_structure,
            Some(crate::capability::select::DiscStructure::BluRay)
        );
        let parts: Vec<(&str, Option<u32>, Option<f64>, &str)> = source
            .parts
            .iter()
            .map(|part| {
                (
                    part.file_id.as_str(),
                    part.part_number,
                    part.duration_secs,
                    part.stream_url.as_str(),
                )
            })
            .collect();
        assert_eq!(
            parts,
            vec![
                (
                    "c1c1c1c1-0000-4000-8000-000000000001",
                    Some(1),
                    Some(3000.0),
                    "https://beam.test/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream"
                ),
                (
                    "c2c2c2c2-0000-4000-8000-000000000002",
                    Some(2),
                    Some(2800.0),
                    "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream"
                ),
            ]
        );
        assert!(source.contains_file("c2c2c2c2-0000-4000-8000-000000000002"));

        let embedded = &source.subtitle_tracks[0];
        assert_eq!(
            embedded.origin,
            crate::capability::select::SubtitleOrigin::Embedded
        );
        assert_eq!(embedded.index, Some(3));
        assert!(!embedded.is_text);
        assert_eq!(
            (embedded.url.as_deref(), embedded.webvtt_url.as_deref()),
            (None, None)
        );

        let sidecar = &source.parts[1].subtitle_tracks[0];
        assert_eq!(
            sidecar.origin,
            crate::capability::select::SubtitleOrigin::Sidecar
        );
        assert_eq!(
            sidecar.sidecar_id.as_deref(),
            Some("5d5d5d5d-0000-4000-8000-000000000002")
        );
        assert_eq!(sidecar.language.as_deref(), Some("spa"));
        assert!(sidecar.is_forced);
        assert_eq!(
            sidecar.webvtt_url.as_deref(),
            Some(
                "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002/webvtt"
            )
        );
        assert_eq!(
            sidecar.url.as_deref(),
            Some(
                "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002"
            )
        );
    }

    /// Two H.264 versions of one title: a 1080p file at 8 Mbps and a 720p
    /// file at 3 Mbps.
    const TWO_H264_SOURCES: &str = r#"{"items":[
        {"file_id":"a1a1a1a1-0000-4000-8000-000000000001","is_primary":true,
         "edition":null,"episode_span":null,"parts":[],
         "size_bytes":4000000000,"mime_type":"video/mp4","container_format":"mp4",
         "duration_secs":6300.0,
         "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                          "frame_rate":null,"bit_rate":8000000,"hdr_format":null}],
         "audio_tracks":[{"index":1,"codec":"aac","language":"eng","title":null,
                          "channels":2,"channel_layout":null,"sample_rate":null,
                          "bit_rate":null,"is_default":true,"is_forced":false}],
         "subtitle_tracks":[],
         "stream_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/stream",
         "download_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/download"},
        {"file_id":"b2b2b2b2-0000-4000-8000-000000000002","is_primary":false,
         "edition":null,"episode_span":null,"parts":[],
         "size_bytes":1500000000,"mime_type":"video/mp4","container_format":"mp4",
         "duration_secs":6300.0,
         "video_tracks":[{"index":0,"codec":"h264","width":1280,"height":720,
                          "frame_rate":null,"bit_rate":3000000,"hdr_format":null}],
         "audio_tracks":[{"index":1,"codec":"aac","language":"eng","title":null,
                          "channels":2,"channel_layout":null,"sample_rate":null,
                          "bit_rate":null,"is_default":true,"is_forced":false}],
         "subtitle_tracks":[],
         "stream_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/stream",
         "download_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/download"}
    ],
    "page_info":{"has_next_page":false,"has_previous_page":false,
                 "start_cursor":null,"end_cursor":null}}"#;

    async fn client_answering_sources(body: &'static str) -> Arc<BeamClient> {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(200, "application/json", body));
        client
            .use_transport(&id, backend as Arc<dyn crate::api::HttpBackend>)
            .expect("the server is registered");
        client
    }

    #[tokio::test]
    async fn a_downgrade_falls_back_from_the_server_sources_to_the_lighter_file() {
        let client = client_answering_sources(TWO_H264_SOURCES).await;
        client.set_device_profile(
            crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
        );

        let downgrade = client
            .select_downgrade_source(
                MEDIA_ID.to_owned(),
                "a1a1a1a1-0000-4000-8000-000000000001".to_owned(),
            )
            .await
            .expect("the sources decode")
            .expect("the 720p file is lighter and plays");

        assert_eq!(
            downgrade.source.file_id,
            "b2b2b2b2-0000-4000-8000-000000000002"
        );
        assert_eq!(
            downgrade.reason,
            "Switched to 720p because playback kept stalling"
        );
    }

    #[tokio::test]
    async fn the_lightest_file_has_no_downgrade_and_that_is_not_an_error() {
        let client = client_answering_sources(TWO_H264_SOURCES).await;
        client.set_device_profile(
            crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
        );

        let downgrade = client
            .select_downgrade_source(
                MEDIA_ID.to_owned(),
                "b2b2b2b2-0000-4000-8000-000000000002".to_owned(),
            )
            .await
            .expect("nothing lighter is an answer, not a failure");

        assert_eq!(downgrade, None);
    }

    #[tokio::test]
    async fn a_downgrade_is_refused_before_the_device_profile_is_known() {
        // Without the profile there is no way to know which smaller file
        // plays, and guessing is how a stall becomes a decoder failure.
        let client = client_answering_sources(TWO_H264_SOURCES).await;

        let refused = client
            .select_downgrade_source(
                MEDIA_ID.to_owned(),
                "a1a1a1a1-0000-4000-8000-000000000001".to_owned(),
            )
            .await;

        assert!(
            matches!(refused, Err(BeamError::BadRequest { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn the_rebuffer_monitor_carries_the_detectors_verdict_across_calls() {
        // The platform holds one monitor per player and calls it from the
        // player's event callbacks; the verdict must accumulate across those
        // calls, and `reset` must clear it for the next file.
        let monitor = RebufferMonitor::new();
        assert!(!monitor.stall_started(0));
        assert!(!monitor.stall_ended(1_000));
        assert!(!monitor.stall_started(10_000));
        assert!(!monitor.stall_ended(11_000));
        assert!(monitor.stall_started(20_000), "the third stall in a minute");
        assert!(monitor.poll(20_500));

        monitor.reset();
        assert!(!monitor.poll(21_000));
    }

    /// A cookie found in secret storage at startup is registered with the
    /// client, not merely remembered: the first request after a cold start
    /// carries it.
    #[tokio::test]
    async fn a_restored_cookie_is_sent_with_the_first_secured_call() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let summary = first
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added");
        storage
            .put_secret(
                format!("session/{}", summary.id),
                "restored-value".to_owned(),
            )
            .await
            .expect("stored");

        let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
        second.restore().await.expect("restored");
        let backend = Arc::new(CannedBackend::answering(
            200,
            "application/json",
            NO_SOURCES,
        ));
        second
            .use_transport(
                &summary.id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        second
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect("an empty list decodes");

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            cookies_of(&recorded[0]),
            vec!["beam_session=restored-value"]
        );
    }

    /// Everything the machine's `UnauthorizedObserved` transition promises, in
    /// one place: the state, the client's credential, the player's copy, and
    /// the stored one. Leaving any behind means a cookie the server has
    /// already refused is sent again -- by the next call, by Media3, or by the
    /// next cold start.
    #[tokio::test]
    async fn a_401_mid_session_withdraws_the_credential_everywhere() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
        let secret_key = format!("session/{id}");
        assert!(storage.has_secret(&secret_key), "signing in stored it");
        let backend = Arc::new(CannedBackend::answering(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401,"detail":"session expired"}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 401 fails the call");

        assert_eq!(error, BeamError::SessionExpired);
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::Expired
        ));
        let sent_before = backend.recorded().len();
        let _ = client.media_sources(MEDIA_ID.to_owned()).await;
        assert_eq!(
            backend.recorded().len(),
            sent_before,
            "the rejected cookie is not sent again"
        );
        assert!(
            matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
            "nor handed to the player"
        );
        assert!(
            !storage.has_secret(&secret_key),
            "nor kept for the next cold start"
        );
    }

    /// The three paths that decide the credential but were covered by no test
    /// of their own. Each answers one question: is the cookie on the client
    /// afterwards -- observed as whether a secured call leaves at all.
    ///
    /// A cookie the server rejected at sign-in must not stay registered, or
    /// every later call would send it and fail in a way the user cannot see.
    #[tokio::test]
    async fn a_rejected_login_leaves_no_credential_behind() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let id = client
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added")
            .id;
        // 403 rather than 401, so that only `complete_login`'s own failure
        // branch withdraws the cookie -- a 401 would have the expiry path do
        // it too, and this test would not know which one did.
        let backend = Arc::new(CannedBackend::answering(
            403,
            "application/problem+json",
            r#"{"type":"about:blank","status":403,"detail":"disabled"}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .complete_login(id.clone(), "refused-value".to_owned())
            .await
            .expect_err("the canned 403 rejects the cookie");

        assert!(matches!(error, BeamError::Forbidden { .. }), "{error:?}");
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::LoggedOut
        ));
        let sent_before = backend.recorded().len();
        assert_eq!(
            client.media_sources(MEDIA_ID.to_owned()).await,
            Err(BeamError::Unauthenticated)
        );
        assert_eq!(
            backend.recorded().len(),
            sent_before,
            "the refused cookie is not on the client"
        );
        assert!(!storage.has_secret(&format!("session/{id}")));
    }

    const DEVICE_START: &str = r#"{"device_handle":"handle-1","user_code":"BCDF-GHJK",
        "verification_uri":"https://idp.test/device",
        "verification_uri_complete":"https://idp.test/device?user_code=BCDF-GHJK",
        "expires_in_secs":600,"interval_secs":5}"#;

    const DEVICE_APPROVED: &str = r#"{"session_token":"issued-value","session_expires_in_secs":5184000,
        "user":{"id":"00000000-0000-0000-0000-0000000000a1","display_name":"Ada","is_admin":false}}"#;

    /// A registered, signed-out server over `backend`.
    async fn device_client(
        backend: CannedBackend,
    ) -> (
        Arc<BeamClient>,
        String,
        Arc<CannedBackend>,
        Arc<InMemoryKeyValueStore>,
    ) {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
        let id = client
            .add_server("https://beam.test".to_owned(), None)
            .await
            .expect("added")
            .id;
        let backend = Arc::new(backend);
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        (client, id, backend, storage)
    }

    /// Starting needs no session: a device with no browser has none yet.
    #[tokio::test]
    async fn a_device_login_starts_signed_out_and_carries_the_prompt_through() {
        let (client, id, backend, _) = device_client(CannedBackend::answering(
            200,
            "application/json",
            DEVICE_START,
        ))
        .await;

        let prompt = client
            .start_device_login(id.clone())
            .await
            .expect("the canned start is a prompt");

        assert_eq!(
            prompt,
            DeviceLoginPrompt {
                device_handle: "handle-1".to_owned(),
                user_code: "BCDF-GHJK".to_owned(),
                verification_uri: "https://idp.test/device".to_owned(),
                verification_uri_complete: Some(
                    "https://idp.test/device?user_code=BCDF-GHJK".to_owned()
                ),
                expires_in_secs: 600,
                interval_secs: 5,
            }
        );
        let recorded = backend.recorded();
        assert_eq!(recorded[0].method, reqwest::Method::POST);
        assert_eq!(recorded[0].url.path(), "/v1/auth/device");
        assert!(
            recorded[0].headers.get(reqwest::header::ORIGIN).is_none(),
            "a native client sends no Origin, which the server's CSRF check allows"
        );
    }

    #[tokio::test]
    async fn a_pending_poll_is_waiting_and_says_whether_to_slow_down() {
        for (body, slow_down) in [
            (
                r#"{"status":"authorization_pending","interval_secs":5}"#,
                false,
            ),
            (r#"{"status":"slow_down","interval_secs":10}"#, true),
        ] {
            let (client, id, _, storage) =
                device_client(CannedBackend::answering(202, "application/json", body)).await;

            let step = client
                .poll_device_login(id.clone(), "handle-1".to_owned())
                .await
                .expect("a 202 is a step, not an error");

            assert_eq!(
                step,
                DeviceLoginStep::Waiting {
                    interval_secs: if slow_down { 10 } else { 5 },
                    slow_down,
                }
            );
            assert!(!storage.has_secret(&format!("session/{id}")));
        }
    }

    /// An approval becomes this server's session exactly as a lifted cookie
    /// does: installed, confirmed, persisted -- and then sent.
    #[tokio::test]
    async fn an_approved_device_login_installs_and_persists_the_issued_credential() {
        let (client, id, backend, storage) = device_client(CannedBackend::answering_in_turn(vec![
            (200, "application/json", DEVICE_APPROVED),
            (200, "application/json", ME),
        ]))
        .await;

        let step = client
            .poll_device_login(id.clone(), "handle-1".to_owned())
            .await
            .expect("approved");

        let DeviceLoginStep::SignedIn { user } = step else {
            panic!("expected a signed-in step, got {step:?}");
        };
        assert_eq!(user.id, "00000000-0000-0000-0000-0000000000a1");
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::Authenticated { .. }
        ));
        assert!(
            storage.has_secret(&format!("session/{id}")),
            "the issued credential survives a restart"
        );

        let recorded = backend.recorded();
        assert_eq!(recorded[0].url.path(), "/v1/auth/device/token");
        assert!(
            cookies_of(&recorded[0]).is_empty(),
            "the poll itself carries no session"
        );
        assert_eq!(recorded[1].url.path(), "/v1/me");
        assert_eq!(cookies_of(&recorded[1]), vec!["beam_session=issued-value"]);
    }

    #[tokio::test]
    async fn a_refused_device_login_is_forbidden_and_stores_nothing() {
        let (client, id, _, storage) = device_client(CannedBackend::answering(
            403,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-denied","status":403,"detail":"refused"}"#,
        ))
        .await;

        let error = client
            .poll_device_login(id.clone(), "handle-1".to_owned())
            .await
            .expect_err("a denial ends the flow");

        assert!(
            matches!(&error, BeamError::Forbidden { code, .. } if code.ends_with("#device-login-denied")),
            "{error:?}"
        );
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::LoggedOut
        ));
        assert!(!storage.has_secret(&format!("session/{id}")));
    }

    /// The fallback cue: a server whose identity provider has no device
    /// grant says so with a 501 that no retry will change.
    #[tokio::test]
    async fn an_unsupported_device_login_is_a_permanent_501() {
        let (client, id, _, _) = device_client(CannedBackend::answering(
            501,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-unsupported","status":501,"detail":"no grant"}"#,
        ))
        .await;

        let error = client
            .start_device_login(id)
            .await
            .expect_err("unsupported");

        assert!(
            matches!(
                &error,
                BeamError::Server { status: 501, retryable: false, code, .. }
                    if code.ends_with("#device-login-unsupported")
            ),
            "{error:?}"
        );
    }

    /// Signing out everywhere ends this device's session too, credential
    /// included.
    #[tokio::test]
    async fn logging_out_everywhere_withdraws_this_devices_credential() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
        let backend = Arc::new(CannedBackend::answering(204, "text/plain", ""));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        client
            .logout_everywhere()
            .await
            .expect("the canned 204 is the server's yes");

        assert_eq!(backend.recorded().len(), 1, "the logout-all itself");
        assert_eq!(
            client.media_sources(MEDIA_ID.to_owned()).await,
            Err(BeamError::Unauthenticated)
        );
        assert_eq!(
            backend.recorded().len(),
            1,
            "nothing else reaches the transport without a credential"
        );
        assert!(!storage.has_secret(&format!("session/{id}")));
        assert!(matches!(
            client.session_state(id).expect("known"),
            SessionState::LoggedOut
        ));
    }

    /// Withdrawing a certificate is a trust decision and says nothing about
    /// who is signed in. The rebuild `forget_certificates` performs went
    /// through `install`, which derives the state from the cookie alone, so a
    /// signed-in server came back `Expired` -- and the viewer was told their
    /// session had expired for having forgotten a certificate.
    #[tokio::test]
    async fn forgetting_certificates_keeps_the_viewer_signed_in() {
        let (client, id, _) = signed_in_client().await;
        client
            .trust_certificate(id.clone(), "AA:BB".to_owned())
            .await
            .expect("trusted");
        let before = client.session_state(id.clone()).expect("known");
        assert!(
            matches!(before, SessionState::Authenticated { .. }),
            "{before:?}"
        );

        client
            .forget_certificates(id.clone())
            .await
            .expect("forgotten");

        assert_eq!(
            client.session_state(id).expect("known"),
            before,
            "forgetting a certificate is not signing out"
        );
    }

    /// Withdrawing trust rebuilds the client; the session must come with it.
    /// A viewer who forgets a certificate has not signed out.
    #[tokio::test]
    async fn forgetting_certificates_keeps_the_session_on_the_rebuilt_client() {
        let (client, id, _) = signed_in_client().await;
        client
            .trust_certificate(id.clone(), "AA:BB".to_owned())
            .await
            .expect("trusted");

        client
            .forget_certificates(id.clone())
            .await
            .expect("forgotten");

        // `install` put a real transport under the rebuilt client; the canned
        // one goes back in the way production swaps nothing else.
        let backend = Arc::new(CannedBackend::answering(
            200,
            "application/json",
            NO_SOURCES,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect("an empty list decodes");

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(cookies_of(&recorded[0]), vec!["beam_session=opaque-value"]);
        assert!(client.trusted_certificates(id).expect("known").is_empty());
    }

    /// A certificate to drive the machine into `Untrusted` with. Never
    /// verified against anything; the state is what the test is about.
    fn certificate() -> crate::trust::CertificateDetails {
        crate::trust::CertificateDetails {
            sha256_fingerprint: "AA:BB".to_owned(),
            spki_sha256_base64: "c3BraQ==".to_owned(),
            subject: "CN=beam.test".to_owned(),
            issuer: "CN=beam.test".to_owned(),
            not_before_unix: 0,
            not_after_unix: i64::MAX,
            subject_alt_names: vec!["beam.test".to_owned()],
            serial_hex: "01".to_owned(),
            is_self_signed: true,
            is_expired: false,
        }
    }

    /// A 401 observed outside `Authenticated` must withdraw nothing and delete
    /// nothing.
    ///
    /// The machine emits no effects for one -- its own comment says a late 401
    /// "must not clobber a sign-in already in progress" -- and every other
    /// state falls through to that. `map_error` used to withdraw the
    /// credential and `fail` to delete the stored cookie on *any* 401, read
    /// off a flag rather than off the transition, so a late answer to a
    /// cancelled request destroyed whatever session had replaced it.
    #[tokio::test]
    async fn a_401_outside_an_authenticated_session_withdraws_nothing() {
        for reached_by in [
            SessionEvent::LoginStarted,
            SessionEvent::LogoutRequested,
            SessionEvent::UnauthorizedObserved,
            SessionEvent::CertificateRejected(Box::new(certificate())),
        ] {
            let storage = Arc::new(InMemoryKeyValueStore::new());
            let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
            // Applied without performing the effects, so the state moves on
            // while the credential stays in place -- the shape a 401 racing a
            // sign-in or a sign-out actually has.
            let _effects = client
                .apply(&id, reached_by)
                .expect("the server is registered");
            let state = client.session_state(id.clone()).expect("known");

            let backend = Arc::new(CannedBackend::answering(
                401,
                "application/problem+json",
                r#"{"type":"about:blank","status":401}"#,
            ));
            client
                .use_transport(
                    &id,
                    Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
                )
                .expect("the server is registered");

            let error = client
                .media_sources(MEDIA_ID.to_owned())
                .await
                .expect_err("the canned 401 fails the call");

            assert_eq!(
                error,
                BeamError::Unauthenticated,
                "a 401 the machine declined to act on is not an expiry, from {state:?}"
            );
            assert_eq!(
                client.session_state(id.clone()).expect("known"),
                state,
                "the state is left where it was"
            );
            assert!(
                storage.has_secret(&format!("session/{id}")),
                "the stored cookie survives, from {state:?}"
            );
            assert!(
                client.server_http_config().is_ok(),
                "and the player keeps its copy, from {state:?}"
            );
        }
    }

    /// The keystore refusing the delete an expiry asks for is swallowed -- the
    /// caller's answer is still the expired session -- but the cookie is then
    /// still on the device. Restoring it would hand the client back the exact
    /// credential the server has just rejected, so the server is marked and
    /// its stored cookie is not registered again.
    #[tokio::test]
    async fn a_cookie_that_could_not_be_deleted_is_not_registered_again() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
        let secret_key = format!("session/{id}");
        assert!(storage.has_secret(&secret_key), "signing in stored it");

        let backend = Arc::new(CannedBackend::answering(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        // A locked keystore: reads still answer, writes and deletes do not.
        storage.set_failure(FailureMode::FailWrites);

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 401 fails the call");

        assert_eq!(
            error,
            BeamError::SessionExpired,
            "swallowing the delete must not change the caller's answer"
        );
        assert!(
            storage.has_secret(&secret_key),
            "the delete really did fail, which is what the mark is for"
        );

        client
            .restore()
            .await
            .expect("the registry itself is still readable");

        assert!(matches!(
            client.session_state(id).expect("known"),
            SessionState::LoggedOut
        ));
        assert_eq!(
            client.media_sources(MEDIA_ID.to_owned()).await,
            Err(BeamError::Unauthenticated),
            "the rejected cookie is not back on the client"
        );
        assert!(
            matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
            "nor handed to the player"
        );
    }

    /// A 401 whose failure is dropped -- a request that never passed through
    /// [`BeamClient::send`] -- leaves the middleware's flag set with nothing
    /// to consume it.
    ///
    /// The flag belongs to the session that provoked it. Left standing across
    /// a sign-in it was spent on the next unrelated failure, expiring a
    /// credential the server had never rejected and deleting it from the
    /// platform keystore, which is not recoverable.
    #[tokio::test]
    async fn a_401_no_call_consumed_cannot_expire_the_session_that_replaced_it() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;

        let rejecting = Arc::new(CannedBackend::answering(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&rejecting) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        // A request made around `send`, so nothing hands its 401 to the
        // session machine -- the shape a dropped failure has.
        let generated = client.client_for(&id).expect("the server is registered");
        let dropped = TransportFailure::capture(
            generated.get_media_detail(parse_uuid("media_id", MEDIA_ID).expect("a UUID")),
        )
        .await;
        assert!(
            dropped.is_err(),
            "the 401 is dropped, which is what leaves the flag unconsumed"
        );

        // The user signs in again: a new cookie, which no server has rejected.
        let confirming = Arc::new(CannedBackend::answering(200, "application/json", ME));
        client
            .use_transport(
                &id,
                Arc::clone(&confirming) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        client
            .complete_login(id.clone(), "second-value".to_owned())
            .await
            .expect("the canned /v1/me confirms the new cookie");

        // Then something entirely unrelated fails under it.
        let missing = Arc::new(CannedBackend::answering(
            404,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404,"detail":"no such title"}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&missing) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 404 fails the call");

        assert!(matches!(error, BeamError::NotFound { .. }), "{error:?}");
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::Authenticated { .. }
        ));
        assert!(
            storage.has_secret(&format!("session/{id}")),
            "the second session is still on the device"
        );
        assert!(client.server_http_config().is_ok());
    }

    /// The reason the queue exists: an interrupted session must not lose the
    /// resume point. A 401 is not retryable *now*, and used to be dropped for
    /// it -- contradicting `SessionExpired`'s own doc comment and both
    /// platform reporters, which tell the viewer their place is safe.
    #[tokio::test]
    async fn a_progress_sample_refused_for_credentials_is_queued_not_dropped() {
        let (client, id) = client_answering(
            401,
            r#"{"type":"about:blank","status":401,"detail":"no session"}"#,
        )
        .await;

        let outcome = client
            .report_progress(uuid::Uuid::nil().to_string(), 120.0, Some(7200.0), true)
            .await
            .expect("a failed send is an outcome, not an error");

        assert_eq!(outcome, ProgressOutcome::Queued { pending: 1 });
        assert_eq!(
            client.pending_progress_count().await.expect("countable"),
            1,
            "the sample is on disk, waiting for the sign-in"
        );
        assert!(
            matches!(
                client.session_state(id).expect("known"),
                SessionState::Expired
            ),
            "a 401 with a session in place is that session expiring"
        );
    }

    /// A flush is a background call, and a 401 during one is a sign-out: the
    /// credential comes off the client and the stored cookie is deleted. It
    /// used to answer `Ok(0)` and leave the caller with no way to know.
    #[tokio::test]
    async fn a_flush_that_expires_the_session_reports_it_rather_than_a_count() {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
        let file_id = uuid::Uuid::nil().to_string();

        // Queued by the ordinary path, against a failure that leaves the
        // session alone -- so the expiry below is the flush's own doing.
        client
            .use_transport(
                &id,
                Arc::new(CannedBackend::answering(
                    503,
                    "application/problem+json",
                    r#"{"type":"about:blank","status":503,"detail":"restarting"}"#,
                )),
            )
            .expect("the server is registered");
        assert_eq!(
            client
                .report_progress(file_id, 120.0, Some(7200.0), true)
                .await
                .expect("a failed send is an outcome, not an error"),
            ProgressOutcome::Queued { pending: 1 }
        );

        client
            .use_transport(
                &id,
                Arc::new(CannedBackend::answering(
                    401,
                    "application/problem+json",
                    r#"{"type":"about:blank","status":401}"#,
                )),
            )
            .expect("the server is registered");

        let error = client
            .flush_progress()
            .await
            .expect_err("a teardown is not a count");

        assert_eq!(error, BeamError::SessionExpired);
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::Expired
        ));
        assert!(
            !storage.has_secret(&format!("session/{id}")),
            "the flush really did sign the device out"
        );
        assert_eq!(
            client.pending_progress_count().await.expect("countable"),
            1,
            "and the resume point is still waiting for the sign-in"
        );
    }

    /// The other half: a request the server refuses on its shape fails
    /// identically forever, and a queue slot spent on it never retires --
    /// `enqueue` replaces the entry and resets `attempts` every fifteen
    /// seconds while the title plays.
    #[tokio::test]
    async fn a_progress_sample_the_server_refuses_is_dropped() {
        let (client, _) = client_answering(
            400,
            r#"{"type":"about:blank","status":400,"detail":"position_secs must be finite"}"#,
        )
        .await;

        let outcome = client
            .report_progress(uuid::Uuid::nil().to_string(), 120.0, Some(7200.0), true)
            .await
            .expect("a refused send is an outcome, not an error");

        let ProgressOutcome::Dropped { reason } = outcome else {
            panic!("expected the sample to be dropped, got {outcome:?}");
        };
        assert!(reason.contains("position_secs must be finite"), "{reason}");
        assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
    }

    /// A problem `type` the contract does not list for the status is lost,
    /// status and all.
    ///
    /// This pins a gap, not a preference: the generated client decodes the
    /// `type` as a closed set and reports anything else as a decode failure
    /// that carries no status, so a code the server adds after a client ships
    /// turns a documented 404 into a malformed response (getkono/spargen#268).
    /// Expected to fail when that is fixed -- the failure is the reminder to
    /// classify this as the 404 it is.
    #[tokio::test]
    async fn a_problem_type_the_contract_does_not_list_loses_its_status() {
        let (client, _) = client_answering(
            404,
            r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404}"#,
        )
        .await;

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 404 fails the call");

        assert!(matches!(error, BeamError::Protocol { .. }), "{error:?}");
    }

    /// A scan is accepted, not finished: the 202 and its job are all the
    /// call waits for.
    #[tokio::test]
    async fn starting_a_scan_returns_once_the_server_accepts_it() {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(
            202,
            "application/json",
            r#"{"id":"00000000-0000-0000-0000-000000000001","library_id":"00000000-0000-0000-0000-000000000002","trigger":"manual","state":"queued","queued_at":"2026-09-27T00:00:00Z","started_at":null,"finished_at":null,"progress":{"total_count":null,"processed_count":0,"added_count":0,"changed_count":0,"unchanged_count":0,"deferred_count":0,"failed_count":0,"marked_missing_count":0,"restored_count":0,"relinked_count":0,"purged_count":0},"failure":null}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        client
            .scan_library("00000000-0000-0000-0000-000000000002".to_owned())
            .await
            .expect("an accepted scan is a success");

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, reqwest::Method::POST);
        assert!(
            recorded[0]
                .url
                .path()
                .ends_with("/v1/admin/libraries/00000000-0000-0000-0000-000000000002/scan"),
            "{}",
            recorded[0].url
        );
    }

    /// A library id that is not a UUID is refused before anything is sent:
    /// the route captures a `Uuid`, so the request could only be a 400.
    #[tokio::test]
    async fn a_scan_of_a_malformed_library_id_is_refused_without_a_request() {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(202, "application/json", "{}"));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .scan_library("not-a-uuid".to_owned())
            .await
            .expect_err("a malformed id fails the call");

        assert!(
            matches!(&error, BeamError::BadRequest { code, .. } if code == ABOUT_BLANK),
            "{error:?}"
        );
        assert!(backend.recorded().is_empty(), "nothing was sent");
    }

    /// A scan already running is the server's 409, surfaced with its problem
    /// type so a screen can say so rather than report a failure.
    #[tokio::test]
    async fn a_scan_already_running_is_a_409() {
        let (client, id, _) = signed_in_client().await;
        client
            .use_transport(
                &id,
                Arc::new(CannedBackend::answering(
                    409,
                    "application/problem+json",
                    r#"{"type":"https://beam.justinchung.net/reference/errors/#library-scan-in-progress","status":409,"detail":"A scan of this library is already queued or running"}"#,
                )),
            )
            .expect("the server is registered");

        let error = client
            .scan_library("00000000-0000-0000-0000-000000000002".to_owned())
            .await
            .expect_err("the canned 409 fails the call");

        assert!(
            matches!(
                &error,
                BeamError::Server { status: 409, code, .. }
                    if code.ends_with("#library-scan-in-progress")
            ),
            "{error:?}"
        );
    }

    /// A 429 whose body is not Beam's `rate-limited` problem -- a reverse
    /// proxy's own limiter answering in HTML -- is lost the same way.
    ///
    /// `browse_media` declares its 429 as that problem, so the generated
    /// client decodes the body, fails, and reports a decode failure with no
    /// status: the caller sees a malformed response rather than
    /// `RateLimited`, and loses `Retry-After` (getkono/spargen#268). Expected
    /// to fail when that is fixed.
    #[tokio::test]
    async fn a_429_a_proxy_wrote_is_not_recognised_as_rate_limiting() {
        let (client, id, _) = signed_in_client().await;
        client
            .use_transport(
                &id,
                Arc::new(CannedBackend::answering(
                    429,
                    "text/html",
                    "<html><body>Too Many Requests</body></html>",
                )),
            )
            .expect("the server is registered");

        let error = client
            .browse_media(BrowseQuery::default())
            .await
            .expect_err("the canned 429 fails the call");

        assert!(matches!(error, BeamError::Protocol { .. }), "{error:?}");
    }

    /// The document the middleware captured is the one the error carries.
    ///
    /// Two 404s a viewer must be told about differently is what issue #123
    /// opened with; that only works if the `type` survives the trip from the
    /// scope into `classify`.
    #[tokio::test]
    async fn a_failed_call_carries_its_own_problem_document_into_the_error() {
        let (client, _) = client_answering(
            404,
            r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404,"detail":"Media not found"}"#,
        )
        .await;

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 404 fails the call");

        assert_eq!(
            error,
            BeamError::NotFound {
                detail: "Media not found".to_owned(),
                code: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
            }
        );
    }

    fn failure(
        kind: FailureKind,
        problem: Option<crate::transport::ProblemDetail>,
    ) -> TransportFailure {
        TransportFailure {
            kind,
            retry_after_secs: None,
            message: "the transport's own words".to_owned(),
            problem,
        }
    }

    /// The shapes a failure can take that classify from the response alone,
    /// with and without a document.
    ///
    /// Only an answered request has a document to classify from; the others
    /// never saw a response, so a document on them would be a bug upstream and
    /// is ignored rather than trusted. `NoCredential` is the one kind that is
    /// read against the session instead, and has its own test below.
    #[tokio::test]
    async fn map_error_classifies_from_the_status_and_the_document_it_was_given() {
        let (client, id) = client_with_server().await;
        let document = crate::transport::ProblemDetail {
            type_uri: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
            detail: Some("media 7 not found".to_owned()),
            pointers: Vec::new(),
        };

        assert_eq!(
            client.map_error(
                &id,
                &failure(FailureKind::Answered(404), Some(document.clone()))
            ),
            BeamError::NotFound {
                detail: "media 7 not found".to_owned(),
                code: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
            }
        );
        assert!(
            matches!(
                client.map_error(&id, &failure(FailureKind::Answered(404), None)),
                BeamError::NotFound { code, .. } if code == ABOUT_BLANK
            ),
            "no document means the status is the whole story"
        );

        for problem in [None, Some(document.clone())] {
            assert_eq!(
                client.map_error(&id, &failure(FailureKind::Unreachable, problem.clone())),
                BeamError::Network {
                    detail: "the transport's own words".to_owned(),
                    retryable: true,
                }
            );
            assert_eq!(
                client.map_error(&id, &failure(FailureKind::Malformed, problem.clone())),
                BeamError::Protocol {
                    detail: "the transport's own words".to_owned(),
                }
            );
            assert_eq!(
                client.map_error(&id, &failure(FailureKind::Unbuildable, problem)),
                BeamError::Protocol {
                    detail: "the transport's own words".to_owned(),
                }
            );
        }
    }

    /// A refused credential is read against the session -- which of the two
    /// unauthenticated answers to give -- while a request that genuinely could
    /// not be built is read against nothing at all. The two used to be one
    /// kind told apart by whether a cookie happened to be registered.
    #[tokio::test]
    async fn a_refused_credential_is_read_against_the_session_and_a_failed_build_is_not() {
        let (logged_out, id) = client_with_server().await;
        assert_eq!(
            logged_out.map_error(&id, &failure(FailureKind::NoCredential, None)),
            BeamError::Unauthenticated
        );

        let (signed_in, id, _) = signed_in_client().await;
        signed_in
            .apply(&id, SessionEvent::UnauthorizedObserved)
            .expect("registered");
        assert_eq!(
            signed_in.map_error(&id, &failure(FailureKind::NoCredential, None)),
            BeamError::SessionExpired,
            "a device whose session the server rejected keeps its work in progress"
        );
    }

    /// The credential is read when a request is built, not baked into the
    /// client at construction. Every fan-out holds a client clone taken before
    /// the call it is part of, so a clone that went on carrying a withdrawn
    /// cookie would send it after the sign-out that withdrew it.
    #[tokio::test]
    async fn a_client_taken_before_a_sign_out_stops_carrying_the_cookie() {
        let (client, id, backend) = signed_in_client().await;
        let held = client.client_for(&id).expect("the server is registered");

        client.logout(id).await.expect("logged out");
        let sent_before = backend.recorded().len();

        let refused = held
            .get_current_user()
            .await
            .expect_err("no cookie, no request");

        assert!(
            matches!(refused, crate::api::Error::RequestConstruction(_)),
            "{refused:?}"
        );
        assert_eq!(
            backend.recorded().len(),
            sent_before,
            "the withdrawn cookie must not reach the wire on a client handed out earlier"
        );
    }

    #[tokio::test]
    async fn a_storage_failure_when_adding_a_server_is_reported_not_swallowed() {
        // A server that appears to be added but was never written would come
        // back missing on the next start, with nothing having said so.
        let storage = Arc::new(InMemoryKeyValueStore::new());
        storage.set_failure(FailureMode::FailWrites);
        let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);

        assert!(matches!(
            client
                .add_server("https://beam.test".to_owned(), None)
                .await,
            Err(BeamError::Storage { .. })
        ));
    }

    // -- playback telemetry -----------------------------------------------

    const TELEMETRY_PATH: &str = "/v1/telemetry/playback";

    /// A distinct, well-formed file id per `n`.
    fn telemetry_file(n: u32) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }

    /// A signed-in client whose telemetry requests get these answers in turn.
    async fn telemetry_client(
        answers: Vec<(u16, &'static str, &'static str)>,
    ) -> (Arc<BeamClient>, Arc<CannedBackend>) {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering_in_turn(answers));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        (client, backend)
    }

    /// Every telemetry request's body, in order.
    fn telemetry_bodies(backend: &CannedBackend) -> Vec<serde_json::Value> {
        backend
            .recorded()
            .iter()
            .filter(|request| request.url.path() == TELEMETRY_PATH)
            .map(|request| {
                assert_eq!(request.method, reqwest::Method::POST);
                serde_json::from_slice(&request.body).expect("a JSON body")
            })
            .collect()
    }

    const NO_CONTENT: (u16, &str, &str) = (204, "text/plain", "");

    #[tokio::test]
    async fn telemetry_leaves_at_most_fifty_events_to_a_request() {
        let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;
        client.set_playback_client_kind(TelemetryClientKind::AndroidTv);
        for n in 0..120 {
            client
                .record_playback_start(telemetry_file(n), Some(0))
                .expect("recorded");
        }

        let flush = client.flush_playback_telemetry().await.expect("flushed");

        assert_eq!(
            flush,
            TelemetryFlush {
                sent: 120,
                dropped: 0,
                pending: 0,
                disabled: false
            }
        );
        let bodies = telemetry_bodies(&backend);
        let sizes: Vec<_> = bodies
            .iter()
            .map(|body| body["starts"].as_array().expect("starts").len())
            .collect();
        assert_eq!(sizes, [50, 50, 20]);
        assert_eq!(bodies[0]["client_kind"], "android_tv");
        assert_eq!(
            bodies[0]["starts"][0]["file_id"],
            telemetry_file(0),
            "oldest first"
        );
    }

    #[tokio::test]
    async fn a_409_stops_telemetry_for_the_rest_of_the_session() {
        let (client, backend) = telemetry_client(vec![(
            409,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#playback-telemetry-disabled","status":409}"#,
        )])
        .await;
        for n in 0..60 {
            client
                .record_playback_rebuffer(telemetry_file(n), 2_000)
                .expect("recorded");
        }

        let flush = client.flush_playback_telemetry().await.expect("flushed");

        assert!(flush.disabled);
        assert_eq!((flush.sent, flush.pending), (0, 0));
        assert_eq!(
            telemetry_bodies(&backend).len(),
            1,
            "one refusal is the operator's answer; the rest of the queue is not tried"
        );
        assert_eq!(
            client
                .record_playback_start(telemetry_file(1), None)
                .expect("accepted"),
            0,
            "nothing is held once the server has said no"
        );
        client.flush_playback_telemetry().await.expect("flushed");
        assert_eq!(
            telemetry_bodies(&backend).len(),
            1,
            "and nothing is asked again"
        );
    }

    #[tokio::test]
    async fn a_refused_batch_is_dropped_and_the_flush_moves_on() {
        let (client, backend) = telemetry_client(vec![
            (
                422,
                "application/problem+json",
                r#"{"type":"https://beam.justinchung.net/reference/errors/#validation-failed","status":422,"errors":[{"pointer":"/starts/0/file_id","detail":"bad"}]}"#,
            ),
            NO_CONTENT,
        ])
        .await;
        for n in 0..60 {
            client
                .record_playback_start(telemetry_file(n), None)
                .expect("recorded");
        }

        let flush = client.flush_playback_telemetry().await.expect("flushed");

        assert_eq!(
            flush,
            TelemetryFlush {
                sent: 10,
                dropped: 50,
                pending: 0,
                disabled: false
            }
        );
        assert_eq!(telemetry_bodies(&backend).len(), 2);
    }

    #[tokio::test]
    async fn a_transient_failure_keeps_the_batch_for_the_next_flush() {
        let (client, backend) = telemetry_client(vec![
            (
                503,
                "application/problem+json",
                r#"{"type":"about:blank","status":503}"#,
            ),
            NO_CONTENT,
        ])
        .await;
        for n in 0..60 {
            client
                .record_playback_start(telemetry_file(n), None)
                .expect("recorded");
        }

        let first = client.flush_playback_telemetry().await.expect("flushed");
        assert_eq!((first.sent, first.dropped, first.pending), (0, 0, 60));
        assert_eq!(
            telemetry_bodies(&backend).len(),
            1,
            "an unavailable server is not handed the rest of the backlog"
        );

        let second = client.flush_playback_telemetry().await.expect("flushed");
        assert_eq!((second.sent, second.pending), (60, 0));
        assert_eq!(
            telemetry_bodies(&backend)[1]["starts"][0]["file_id"],
            telemetry_file(0),
            "the kept batch goes first"
        );
    }

    #[tokio::test]
    async fn an_expired_session_ends_the_flush_and_keeps_the_events() {
        let (client, _) = telemetry_client(vec![(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401}"#,
        )])
        .await;
        client
            .record_playback_start(telemetry_file(1), None)
            .expect("recorded");

        assert_eq!(
            client.flush_playback_telemetry().await,
            Err(BeamError::SessionExpired)
        );
        assert_eq!(
            client
                .record_playback_start(telemetry_file(2), None)
                .expect("recorded"),
            2,
            "signing in again delivers what was waiting"
        );
    }

    #[tokio::test]
    async fn uncounted_events_are_accepted_and_change_nothing() {
        let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;

        let excluded = client
            .record_playback_start_failure(
                telemetry_file(1),
                StartFailureCause::Rejected {
                    reason: crate::RejectionReason::ExcludedByPolicy,
                },
                None,
            )
            .expect("accepted");
        let reload = client
            .record_playback_source_switch(
                telemetry_file(1),
                telemetry_file(1),
                SwitchTrigger::Manual,
            )
            .expect("accepted");
        let instant = client
            .record_playback_rebuffer(telemetry_file(1), 0)
            .expect("accepted");

        assert_eq!((excluded, reload, instant), (0, 0, 0));
        client.flush_playback_telemetry().await.expect("flushed");
        assert!(telemetry_bodies(&backend).is_empty(), "nothing to send");
    }

    #[tokio::test]
    async fn a_preflight_rejection_reaches_the_wire_as_one() {
        let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;
        client
            .record_playback_start_failure(
                telemetry_file(1),
                StartFailureCause::Rejected {
                    reason: crate::RejectionReason::ContainerUnsupported,
                },
                Some(1),
            )
            .expect("recorded");

        client.flush_playback_telemetry().await.expect("flushed");

        assert_eq!(
            telemetry_bodies(&backend)[0]["start_failures"][0],
            serde_json::json!({
                "file_id": telemetry_file(1),
                "reason": "container",
                "stage": "preflight",
                "audio_track_index": 1,
            })
        );
    }

    #[tokio::test]
    async fn a_malformed_file_id_is_refused_before_it_is_queued() {
        let (client, _) = telemetry_client(vec![NO_CONTENT]).await;
        assert!(matches!(
            client.record_playback_start("not-a-uuid".to_owned(), None),
            Err(BeamError::BadRequest { .. })
        ));
        assert!(matches!(
            client.record_playback_source_switch(
                telemetry_file(1),
                "nope".to_owned(),
                SwitchTrigger::Auto
            ),
            Err(BeamError::BadRequest { .. })
        ));
    }

    const TELEMETRY_REPORT: &str = r#"{
        "enabled": false,
        "from": "2026-10-01",
        "to": "2026-10-07",
        "retention_days": 400,
        "totals": { "started_count": 4, "failed_count": 1, "rebuffer_count": 0,
                    "rebuffer_total_ms": 0, "source_switch_count": 0 },
        "starts": [{ "client_kind": "android", "container": "mp4", "video_codec": "h264",
                     "audio_codec": "aac", "height_class": "hd", "count": 4 }],
        "start_failures": [{ "client_kind": "android", "reason": "audio_codec",
                             "stage": "preflight", "container": "mp4", "video_codec": "h264",
                             "audio_codec": "dts", "height_class": "hd", "count": 1 }],
        "rebuffers": [],
        "source_switches": []
    }"#;

    #[tokio::test]
    async fn the_admin_telemetry_view_asks_for_the_days_it_was_given() {
        let (client, backend) =
            telemetry_client(vec![(200, "application/json", TELEMETRY_REPORT)]).await;

        let summary = client
            .admin_playback_telemetry(Some("2026-10-01".to_owned()), Some("2026-10-07".to_owned()))
            .await
            .expect("answered");

        let request = backend.recorded().last().cloned().expect("a request");
        assert_eq!(request.url.path(), "/v1/admin/telemetry/playback");
        let query: HashMap<_, _> = request.url.query_pairs().into_owned().collect();
        assert_eq!(query.get("from").map(String::as_str), Some("2026-10-01"));
        assert_eq!(query.get("to").map(String::as_str), Some("2026-10-07"));
        assert!(
            !summary.enabled,
            "counts stay readable after collection is off"
        );
        assert_eq!((summary.started, summary.failed), (4, 1));
        assert_eq!(summary.by_client[0].label, "android");
    }

    #[tokio::test]
    async fn a_day_that_does_not_parse_never_reaches_the_server() {
        let (client, backend) =
            telemetry_client(vec![(200, "application/json", TELEMETRY_REPORT)]).await;
        let sent_before = backend.recorded().len();

        assert!(matches!(
            client
                .admin_playback_telemetry(Some("last tuesday".to_owned()), None)
                .await,
            Err(BeamError::BadRequest { .. })
        ));
        assert_eq!(backend.recorded().len(), sent_before);
    }
}
