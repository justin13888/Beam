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
use crate::clock::{Clock, SystemClock};
use crate::error::BeamError;
use crate::ports::kv::KeyValueStore;
use crate::servers::{ServerRecord, normalize_base_url, server_id_for};
use crate::session::{SessionEvent, SessionState, UserSummary};
use crate::transport::{SessionCookieHolder, SessionMiddleware};
use crate::upnext::{UpNextSeason, next_playable_episode};
use std::collections::HashMap;
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
    /// Certificate pins for this server, as OkHttp `sha256/<base64>` values.
    /// Empty when platform trust already suffices.
    pub certificate_pins: Vec<String>,
    /// The host those pins apply to.
    pub pinned_host: String,
}

/// One server's live state.
struct ServerContext {
    record: ServerRecord,
    client: GeneratedClient,
    cookie: Arc<SessionCookieHolder>,
    middleware: Arc<SessionMiddleware>,
    state: SessionState,
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
            let cookie = self.storage.get_secret(format!("session/{id}")).await?;
            self.install(record, cookie.as_deref())?;
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
            self.persist_record(&record).await?;
            self.install(record, None)?;
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
        self.apply(&server_id, SessionEvent::LoginStarted)?;
        self.with_context(&server_id, |context| {
            context.cookie.set(&session_cookie);
        })?;
        self.apply(&server_id, SessionEvent::CookieCaptured)?;

        match self.fetch_me(&server_id).await {
            Ok(user) => {
                self.storage
                    .put_secret(format!("session/{server_id}"), session_cookie)
                    .await?;
                self.apply(
                    &server_id,
                    SessionEvent::IdentityConfirmed(Box::new(user.clone())),
                )?;
                Ok(user)
            }
            Err(error) => {
                self.with_context(&server_id, |context| context.cookie.clear())?;
                self.storage
                    .remove_secret(format!("session/{server_id}"))
                    .await?;
                self.apply(&server_id, SessionEvent::IdentityRejected)?;
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
        let _ = client.beam_auth_server_oidc_routes_oidc_logout().await;

        self.with_context(&server_id, |context| context.cookie.clear())?;
        self.storage
            .remove_secret(format!("session/{server_id}"))
            .await?;
        self.apply(&server_id, SessionEvent::LogoutRequested)?;
        Ok(())
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

        let response = client
            .beam_server_routes_media_get_media_sources(media_id)
            .await
            .map_err(|error| self.map_error(&server_id, &error.to_string()))?;

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
                message: "the device profile has not been set".to_owned(),
            })?;

        crate::capability::select_source(&sources, &profile, &policy).map_err(|rejections| {
            let detail = rejections.first().map_or_else(
                || "This title has no playable files".to_owned(),
                |first| first.detail.clone(),
            );
            BeamError::NotFound { message: detail }
        })
    }

    /// Everything the platform player needs to stream a file itself.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Unauthenticated`] when there is no session.
    pub fn playback_config(&self, file_id: String) -> Result<PlaybackHttpConfig, BeamError> {
        let server_id = self.require_active()?;
        let servers = self.servers.read().expect("servers lock");
        let context = servers.get(&server_id).ok_or(BeamError::UnknownServer {
            server_id: server_id.clone(),
        })?;

        let cookie = context.cookie.get().ok_or(BeamError::Unauthenticated)?;
        let url = context
            .record
            .absolute_url(&format!("/v1/files/{file_id}/stream"))?;
        let host = url::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_default();

        let mut headers = HashMap::new();
        headers.insert(
            "Cookie".to_owned(),
            format!("{}={cookie}", crate::transport::SESSION_COOKIE),
        );

        Ok(PlaybackHttpConfig {
            url,
            headers,
            certificate_pins: Vec::new(),
            pinned_host: host,
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
}

impl BeamClient {
    fn install(&self, record: ServerRecord, cookie: Option<&str>) -> Result<(), BeamError> {
        let holder = Arc::new(SessionCookieHolder::new());
        if let Some(value) = cookie {
            holder.set(value);
        }
        let middleware = Arc::new(SessionMiddleware::new(Arc::clone(&holder)));

        let http = reqwest::Client::builder()
            .build()
            .map_err(|error| BeamError::Network {
                message: format!("could not build an HTTP client: {error}"),
                retryable: false,
            })?;
        let backend = MiddlewareBackend::with_middlewares(
            Arc::new(ReqwestBackend::new(http)),
            vec![Arc::clone(&middleware) as Arc<dyn crate::api::Middleware>],
        );
        let client = GeneratedClient::with_backend(Arc::new(backend), &record.base_url).map_err(
            |error| BeamError::InvalidServerUrl {
                message: error.to_string(),
            },
        )?;

        // A restored cookie is trusted until a request says otherwise, which
        // is what keeps a cold start off the network.
        let state = if cookie.is_some() {
            SessionState::Expired
        } else {
            SessionState::LoggedOut
        };

        self.servers.write().expect("servers lock").insert(
            record.id.clone(),
            ServerContext {
                record,
                client,
                cookie: holder,
                middleware,
                state,
            },
        );
        Ok(())
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

    fn apply(&self, server_id: &str, event: SessionEvent) -> Result<(), BeamError> {
        let mut servers = self.servers.write().expect("servers lock");
        let context = servers
            .get_mut(server_id)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })?;
        let (next, _effects) = crate::session::transition(&context.state, event);
        context.state = next;
        Ok(())
    }

    fn client_for(&self, server_id: &str) -> Result<GeneratedClient, BeamError> {
        self.with_context(server_id, |context| context.client.clone())
    }

    fn record_for(&self, server_id: &str) -> Result<ServerRecord, BeamError> {
        self.with_context(server_id, |context| context.record.clone())
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
    fn map_error(&self, server_id: &str, message: &str) -> BeamError {
        let saw_401 = self
            .with_context(server_id, |context| context.middleware.take_unauthorized())
            .unwrap_or(false);
        if saw_401 {
            let _ = self.apply(server_id, SessionEvent::UnauthorizedObserved);
            return BeamError::SessionExpired;
        }
        BeamError::Network {
            message: message.to_owned(),
            retryable: true,
        }
    }

    async fn fetch_me(&self, server_id: &str) -> Result<UserSummary, BeamError> {
        let client = self.client_for(server_id)?;
        let response = client
            .beam_auth_server_oidc_routes_oidc_me()
            .await
            .map_err(|error| self.map_error(server_id, &error.to_string()))?;
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
            message: error.to_string(),
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
            message: error.to_string(),
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
    source: crate::api::types::BeamServerModelsMediaSourceMediaSource,
    record: &ServerRecord,
) -> Result<MediaSourceView, BeamError> {
    // JSON Schema has no unsigned integer type, so salvo's `uint32`/`uint64`
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
