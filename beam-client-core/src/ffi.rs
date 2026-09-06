//! The surface Kotlin and Swift actually call.
//!
//! Everything here is a thin shell over the modules beside it. The logic lives
//! in those modules, where it is testable as plain Rust; this file only
//! marshals. Keeping the split strict is what stops UniFFI's generated
//! scaffolding from dominating the crate's coverage, and it means a Kotlin
//! integration test and a Rust unit test are exercising the same code.

use crate::api::Client as GeneratedClient;
use crate::api::{MiddlewareBackend, ReqwestBackend};
use crate::capability::{DeviceProfile, MediaSourceView, QualityPolicy, SourceSelection};
use crate::catalog::{
    AdminEvent, AdminLogEntry, AdminStatus, AdminUser, AdminUserPage, BrowseQuery,
    ContinueWatchingEntry, DeviceSession, EpisodeSummary, HistoryEntry, HistoryPage,
    LibraryFileSummary, LibrarySummary, MediaDetail, MediaPage, ServerHealth, browse_params,
};
use crate::clock::{Clock, SystemClock};
use crate::error::BeamError;
use crate::ports::kv::KeyValueStore;
use crate::progress::{
    ProgressOutcome, ProgressQueue, ProgressThrottle, QueuedProgress, ThrottleDecision,
};
use crate::servers::{ServerRecord, normalize_base_url, server_id_for};
use crate::session::{SessionEffect, SessionEvent, SessionState, UserSummary};
use crate::transport::{ABOUT_BLANK, FailureKind, SessionMiddleware, TransportFailure, classify};
use crate::upnext::{UpNextSeason, next_playable_episode};
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
    /// Titles already resolved for this server.
    ///
    /// The playback endpoints return bare identifiers, so every
    /// continue-watching tile would otherwise cost a detail request on every
    /// refresh. Held behind its own `Arc` so a lookup can be performed without
    /// keeping the server map locked across an await.
    metadata: Arc<RwLock<HashMap<String, MediaDetail>>>,
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
#[derive(Debug, uniffi::Object)]
pub struct BeamClient {
    storage: Arc<dyn KeyValueStore>,
    clock: Arc<dyn Clock>,
    servers: RwLock<HashMap<String, ServerContext>>,
    active: RwLock<Option<String>>,
    device_profile: RwLock<Option<DeviceProfile>>,
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
        let client = self.client_for(&server_id)?;
        // `RevokeRemoteSession`, performed before the transition because the
        // request needs the credential the transition withdraws.
        let _ = client.logout(None).await;

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
            .send(&server_id, client.get_media_sources(media_id, None))
            .await?;

        response
            .into_inner()
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
        let profile = self
            .device_profile
            .read()
            .expect("profile lock")
            .clone()
            .ok_or_else(|| BeamError::BadRequest {
                detail: "the device profile has not been set".to_owned(),
                // Refused here, so there is no server type to carry.
                code: ABOUT_BLANK.to_owned(),
            })?;

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

    /// The next episode to play after this one.
    #[must_use]
    pub fn up_next(
        &self,
        seasons: Vec<UpNextSeason>,
        current_episode_id: String,
    ) -> Option<crate::upnext::UpNextEpisode> {
        next_playable_episode(&seasons, &current_episode_id)
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
    /// The result is cached per server, because the continue-watching and
    /// history screens resolve the same titles repeatedly.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn media_detail(&self, media_id: String) -> Result<MediaDetail, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let cache = self.metadata_cache(&server_id)?;
        match Self::fetch_detail(&client, &record, &cache, &media_id).await {
            Ok(detail) => Ok(detail),
            Err(failure) => Err(self.fail(&server_id, &failure).await),
        }
    }

    /// Every genre the catalog contains, for the explore filters.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn genres(&self) -> Result<Vec<String>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_genres(None)).await?;
        Ok(response.into_inner().genres)
    }

    /// The next playable episode of a series after the given one.
    ///
    /// Resolves the series itself rather than making the caller assemble the
    /// season list, so auto-advance is one call from the player.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn up_next_in_show(
        &self,
        show_id: String,
        current_episode_id: String,
    ) -> Result<Option<EpisodeSummary>, BeamError> {
        let MediaDetail::Show { seasons, .. } = self.media_detail(show_id).await? else {
            return Ok(None);
        };
        let as_up_next: Vec<UpNextSeason> = seasons
            .iter()
            .map(|season| UpNextSeason {
                season_number: i32::try_from(season.season_number).unwrap_or(i32::MAX),
                episodes: season
                    .episodes
                    .iter()
                    .map(|episode| crate::upnext::UpNextEpisode {
                        id: episode.id.clone(),
                        episode_number: i32::try_from(episode.episode_number).unwrap_or(i32::MAX),
                        file_id: episode.file_id.clone(),
                    })
                    .collect(),
            })
            .collect();

        let Some(next) = next_playable_episode(&as_up_next, &current_episode_id) else {
            return Ok(None);
        };
        Ok(seasons
            .into_iter()
            .flat_map(|season| season.episodes)
            .find(|episode| episode.id == next.id))
    }

    // -- libraries --------------------------------------------------------

    /// Every library on the server.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn libraries(&self) -> Result<Vec<LibrarySummary>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_libraries(None)).await?;
        Ok(response
            .into_inner()
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
            .send(&server_id, client.get_library(library_id, None))
            .await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// The files indexed into one library.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn library_files(
        &self,
        library_id: String,
    ) -> Result<Vec<LibraryFileSummary>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(&server_id, client.get_library_files(library_id, None))
            .await?;
        Ok(response
            .into_inner()
            .into_iter()
            .map(LibraryFileSummary::from_generated)
            .collect())
    }

    // -- playback ---------------------------------------------------------

    /// Partially-watched titles, ready to resume, newest first.
    ///
    /// The server returns identifiers only, so the core resolves each title's
    /// metadata before returning -- concurrently, and against the per-server
    /// cache, so a home screen costs one round trip per *distinct* unseen
    /// title rather than one per tile on every refresh.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures. A title whose metadata cannot
    /// be resolved is still returned, with `media` left `None`.
    pub async fn continue_watching(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<ContinueWatchingEntry>, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetContinueWatchingParams {
            limit: limit.map(i64::from),
            origin: None,
            referer: None,
        };
        let response = self
            .send(&server_id, client.get_continue_watching(params))
            .await?;

        let mut entries: Vec<ContinueWatchingEntry> = response
            .into_inner()
            .into_iter()
            .map(ContinueWatchingEntry::from_generated)
            .collect();

        let cache = self.metadata_cache(&server_id)?;
        let ids: Vec<String> = entries.iter().map(|entry| entry.media_id.clone()).collect();
        let resolved = Self::hydrate(&client, &record, &cache, ids).await;
        for entry in &mut entries {
            if let Some(detail) = resolved.get(&entry.media_id) {
                entry.media = Some(detail.summary().clone());
                entry.episode = entry
                    .episode_id
                    .as_deref()
                    .and_then(|id| find_episode(detail, id));
            }
        }
        Ok(entries)
    }

    /// One page of watch history, newest first.
    ///
    /// Hydrated the same way as [`Self::continue_watching`].
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn history(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<HistoryPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::GetHistoryParams {
            limit: limit.map(i64::from),
            offset: offset.map(i64::from),
            origin: None,
            referer: None,
        };
        let response = self
            .send(&server_id, client.get_history(params))
            .await?
            .into_inner();

        let total = u64::try_from(response.total).unwrap_or(0);
        let mut items: Vec<HistoryEntry> = response
            .items
            .into_iter()
            .map(HistoryEntry::from_generated)
            .collect();

        let cache = self.metadata_cache(&server_id)?;
        let ids: Vec<String> = items.iter().map(|entry| entry.media_id.clone()).collect();
        let resolved = Self::hydrate(&client, &record, &cache, ids).await;
        for entry in &mut items {
            if let Some(detail) = resolved.get(&entry.media_id) {
                entry.media = Some(detail.summary().clone());
                entry.episode = entry
                    .episode_id
                    .as_deref()
                    .and_then(|id| find_episode(detail, id));
            }
        }
        Ok(HistoryPage { items, total })
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
        match TransportFailure::capture(client.report_playback_progress(wire_file_id, None, &body))
            .await
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
            match TransportFailure::capture(client.report_playback_progress(
                wire_file_id,
                None,
                &body,
            ))
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

    // -- sessions ---------------------------------------------------------

    /// Every device signed in as this user.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn sessions(&self) -> Result<Vec<DeviceSession>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.list_sessions(None)).await?;
        Ok(response
            .into_inner()
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
        self.send(&server_id, client.delete_session(session_id, None))
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
        self.send(&server_id, client.logout_all(None)).await?;

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
        let response = self.send(&server_id, client.get_admin_status(None)).await?;
        Ok(AdminStatus::from_generated(response.into_inner()))
    }

    /// One page of user accounts.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_users(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<AdminUserPage, BeamError> {
        let (server_id, client, record) = self.active_context()?;
        let params = crate::api::ListAdminUsersParams {
            limit: limit.map(i64::from),
            offset: offset.map(i64::from),
            origin: None,
            referer: None,
        };
        let response = self
            .send(&server_id, client.list_admin_users(params))
            .await?
            .into_inner();
        Ok(AdminUserPage {
            total: u64::try_from(response.total).unwrap_or(0),
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
        self.send(
            &server_id,
            client.update_admin_user(wire_user_id, None, &body),
        )
        .await?;
        Ok(())
    }

    /// One page of the operational log.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_logs(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<AdminLogEntry>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let params = crate::api::GetAdminLogsParams {
            limit: limit.map(i64::from),
            offset: offset.map(i64::from),
            origin: None,
            referer: None,
        };
        let response = self.send(&server_id, client.get_admin_logs(params)).await?;
        Ok(response
            .into_inner()
            .into_iter()
            .map(AdminLogEntry::from_generated)
            .collect())
    }

    /// How many log lines the server holds.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_log_count(&self) -> Result<u64, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(&server_id, client.get_admin_log_count(None))
            .await?;
        Ok(u64::try_from(response.into_inner().count).unwrap_or(0))
    }

    /// Recent server events, newest first.
    ///
    /// This polls `GET /v1/admin/events` rather than subscribing to
    /// `/v1/admin/events/stream`. Kynos now describes the streaming endpoint
    /// with OpenAPI 3.2's `itemSchema`, and spargen lowers it to
    /// `Client::stream_admin_events` returning
    /// `support::EventStream<types::AdminEventDto>`. Nothing on the UniFFI
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
            limit: limit.map(i64::from),
            origin: None,
            referer: None,
        };
        let response = self
            .send(&server_id, client.get_admin_events(params))
            .await?;
        Ok(response
            .into_inner()
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
        let response = self
            .send(&server_id, client.create_library(None, &body))
            .await?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// Delete a library and everything indexed into it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn delete_library(&self, library_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(&server_id, client.delete_library(library_id, None))
            .await?;
        Ok(())
    }

    /// Rescan a library, returning how many files were added.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn scan_library(&self, library_id: String) -> Result<u32, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(&server_id, client.scan_library(library_id, None))
            .await?;
        Ok(u32::try_from(response.into_inner().added).unwrap_or(0))
    }

    /// Re-fetch metadata for one title.
    ///
    /// Evicts the core's cached copy, so the next read reflects the refresh
    /// rather than serving what the screen was already showing.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn refresh_media_metadata(&self, media_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        self.send(
            &server_id,
            client.refresh_media_metadata(media_id.clone(), None),
        )
        .await?;
        if let Ok(cache) = self.metadata_cache(&server_id) {
            cache.write().expect("metadata lock").remove(&media_id);
        }
        Ok(())
    }

    /// The server's own health report.
    ///
    /// # Errors
    ///
    /// Propagates transport failures.
    pub async fn health(&self) -> Result<ServerHealth, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self.send(&server_id, client.get_health(None)).await?;
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

/// The episode with this id, wherever it sits in a series.
fn find_episode(detail: &MediaDetail, episode_id: &str) -> Option<EpisodeSummary> {
    let MediaDetail::Show { seasons, .. } = detail else {
        return None;
    };
    seasons
        .iter()
        .flat_map(|season| &season.episodes)
        .find(|episode| episode.id == episode_id)
        .cloned()
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
            metadata: Arc::new(RwLock::new(HashMap::new())),
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

    /// The metadata cache belonging to one server.
    fn metadata_cache(
        &self,
        server_id: &str,
    ) -> Result<Arc<RwLock<HashMap<String, MediaDetail>>>, BeamError> {
        self.with_context(server_id, |context| Arc::clone(&context.metadata))
    }

    /// One title, from the cache when it is there and from the server when it
    /// is not.
    async fn fetch_detail(
        client: &GeneratedClient,
        record: &ServerRecord,
        cache: &Arc<RwLock<HashMap<String, MediaDetail>>>,
        media_id: &str,
    ) -> Result<MediaDetail, TransportFailure> {
        if let Some(hit) = cache.read().expect("metadata lock").get(media_id) {
            return Ok(hit.clone());
        }
        let response =
            TransportFailure::capture(client.get_media_detail(media_id.to_owned(), None)).await?;
        let detail = MediaDetail::from_generated(response.into_inner(), record);
        cache
            .write()
            .expect("metadata lock")
            .insert(media_id.to_owned(), detail.clone());
        Ok(detail)
    }

    /// Resolve many titles at once, de-duplicated.
    ///
    /// Failures are dropped rather than propagated: a continue-watching row
    /// whose artwork could not be fetched is still a valid place to resume
    /// from, and failing the whole screen over one of them would be worse than
    /// showing it plainly.
    async fn hydrate(
        client: &GeneratedClient,
        record: &ServerRecord,
        cache: &Arc<RwLock<HashMap<String, MediaDetail>>>,
        media_ids: Vec<String>,
    ) -> HashMap<String, MediaDetail> {
        let mut wanted: Vec<String> = media_ids;
        wanted.sort_unstable();
        wanted.dedup();

        let mut tasks = tokio::task::JoinSet::new();
        for media_id in wanted {
            let client = client.clone();
            let record = record.clone();
            let cache = Arc::clone(cache);
            tasks.spawn(async move {
                let detail = Self::fetch_detail(&client, &record, &cache, &media_id).await;
                (media_id, detail)
            });
        }

        let mut resolved = HashMap::new();
        while let Some(joined) = tasks.join_next().await {
            if let Ok((media_id, Ok(detail))) = joined {
                resolved.insert(media_id, detail);
            }
        }
        resolved
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
        let response = self.send(server_id, client.get_current_user(None)).await?;
        let me = response.into_inner();
        Ok(UserSummary {
            id: me.id,
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
    let video = source.video;
    Ok(MediaSourceView {
        file_id: source.file_id,
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::kv::{FailureMode, InMemoryKeyValueStore};
    use crate::transport::CannedBackend;
    use crate::upnext::{UpNextEpisode, UpNextSeason};

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

    #[tokio::test]
    async fn up_next_is_resolved_locally_from_the_seasons_it_is_given() {
        let client = client();
        let seasons = vec![UpNextSeason {
            season_number: 1,
            episodes: vec![
                UpNextEpisode {
                    id: "e1".to_owned(),
                    episode_number: 1,
                    file_id: Some("f1".to_owned()),
                },
                UpNextEpisode {
                    id: "e2".to_owned(),
                    episode_number: 2,
                    file_id: Some("f2".to_owned()),
                },
            ],
        }];

        let next = client.up_next(seasons, "e1".to_owned());

        assert_eq!(next.map(|episode| episode.id), Some("e2".to_owned()));
    }

    /// What `GET /v1/me` answers for the signed-in user in these tests.
    const ME: &str = r#"{"id":"u-1","display_name":"Ada","is_admin":false}"#;

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

        // The canned body is a `MeResponse`, so this decodes to nothing
        // useful; what matters is that the request left at all, and how.
        let _ = client.media_sources("7".to_owned()).await;

        let recorded = backend.recorded();
        assert_eq!(recorded.len(), 2, "the /v1/me of the sign-in, then this");
        let sources = &recorded[1];
        assert_eq!(sources.method, reqwest::Method::GET);
        assert_eq!(sources.url.path(), "/v1/media/7/sources");
        assert_eq!(cookies_of(sources), vec!["beam_session=opaque-value"]);
        assert_eq!(
            cookies_of(&recorded[0]),
            vec!["beam_session=opaque-value"],
            "the sign-in's own /v1/me is a secured call too"
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
            .media_sources("7".to_owned())
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
        let _ = client.media_sources("7".to_owned()).await;
        let sent_before = backend.recorded().len();

        let error = client
            .media_sources("7".to_owned())
            .await
            .expect_err("no credential, no request");

        assert_eq!(error, BeamError::SessionExpired);
        assert!(!error.is_permanent(), "{error:?}");
        assert_eq!(backend.recorded().len(), sent_before);
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
        let backend = Arc::new(CannedBackend::answering(200, "application/json", "[]"));
        second
            .use_transport(
                &summary.id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        second
            .media_sources("7".to_owned())
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
            .media_sources("7".to_owned())
            .await
            .expect_err("the canned 401 fails the call");

        assert_eq!(error, BeamError::SessionExpired);
        assert!(matches!(
            client.session_state(id.clone()).expect("known"),
            SessionState::Expired
        ));
        let sent_before = backend.recorded().len();
        let _ = client.media_sources("7".to_owned()).await;
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
            client.media_sources("7".to_owned()).await,
            Err(BeamError::Unauthenticated)
        );
        assert_eq!(
            backend.recorded().len(),
            sent_before,
            "the refused cookie is not on the client"
        );
        assert!(!storage.has_secret(&format!("session/{id}")));
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
            client.media_sources("7".to_owned()).await,
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
        let backend = Arc::new(CannedBackend::answering(200, "application/json", "[]"));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");
        client
            .media_sources("7".to_owned())
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
                .media_sources("7".to_owned())
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
            .media_sources("7".to_owned())
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
            client.media_sources("7".to_owned()).await,
            Err(BeamError::Unauthenticated),
            "the rejected cookie is not back on the client"
        );
        assert!(
            matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
            "nor handed to the player"
        );
    }

    /// A 401 whose failure is dropped -- `hydrate` shows a continue-watching
    /// row plainly rather than failing the whole screen over one unresolved
    /// title -- leaves the middleware's flag set with nothing to consume it.
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
        let resolved = BeamClient::hydrate(
            &client.client_for(&id).expect("the server is registered"),
            &client.record_for(&id).expect("the server is registered"),
            &client
                .metadata_cache(&id)
                .expect("the server is registered"),
            vec!["7".to_owned()],
        )
        .await;
        assert!(
            resolved.is_empty(),
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
            r#"{"type":"about:blank","status":404,"detail":"no such title"}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&missing) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .media_sources("7".to_owned())
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
            r#"{"type":"https://beam.justinchung.net/reference/errors/#validation","status":400,"detail":"position_secs must be finite"}"#,
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

    /// The document the middleware captured is the one the error carries.
    ///
    /// Two 404s a viewer must be told about differently is what issue #123
    /// opened with; that only works if the `type` survives the trip from the
    /// scope into `classify`.
    #[tokio::test]
    async fn a_failed_call_carries_its_own_problem_document_into_the_error() {
        let (client, _) = client_answering(
            404,
            r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404,"detail":"Source video file not found"}"#,
        )
        .await;

        let error = client
            .media_sources("7".to_owned())
            .await
            .expect_err("the canned 404 fails the call");

        assert_eq!(
            error,
            BeamError::NotFound {
                detail: "Source video file not found".to_owned(),
                code: "https://beam.justinchung.net/reference/errors/#source-file-missing"
                    .to_owned(),
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
            .get_current_user(None)
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
}
