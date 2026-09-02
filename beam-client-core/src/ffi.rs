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
use crate::session::{SessionEvent, SessionState, UserSummary};
use crate::transport::{
    ABOUT_BLANK, FailureKind, SessionCookieHolder, SessionMiddleware, TransportFailure, classify,
};
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

/// One server's live state.
struct ServerContext {
    record: ServerRecord,
    client: GeneratedClient,
    cookie: Arc<SessionCookieHolder>,
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
            // Installed before persisting, because `persist_record` writes the
            // index from the in-memory map. Persisting first wrote an index
            // that did not yet contain this server, so the very first server
            // added was never in it -- and `restore` found nothing, sending
            // the viewer back to the address field on every cold start.
            self.install(record.clone(), None)?;
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
        let _ = client.logout(None).await;

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

        let response = TransportFailure::capture(client.get_media_sources(media_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;

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
        let (record, cookie) = {
            let servers = self.servers.read().expect("servers lock");
            let context = servers
                .get(&server_id)
                .ok_or_else(|| BeamError::UnknownServer {
                    server_id: server_id.clone(),
                })?;
            let mut record = context.record.clone();
            record.trusted_fingerprints.clear();
            (record, context.cookie.get())
        };
        self.install(record.clone(), cookie.as_deref())?;
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
        let response = TransportFailure::capture(client.browse_media(browse_params(&query)?))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        Self::fetch_detail(&client, &record, &cache, &media_id)
            .await
            .map_err(|error| self.map_error(&server_id, &error))
    }

    /// Every genre the catalog contains, for the explore filters.
    ///
    /// # Errors
    ///
    /// Propagates transport and server failures.
    pub async fn genres(&self) -> Result<Vec<String>, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = TransportFailure::capture(client.list_genres(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.list_libraries(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_library(library_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_library_files(library_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_continue_watching(params))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;

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
        let response = TransportFailure::capture(client.get_history(params))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?
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
                let mapped = self.map_error(&server_id, &failure);

                // A rejected body, a forbidden file, a title that no longer
                // exists: the identical request fails identically forever.
                // Queuing one occupied a slot in a bounded queue and never
                // retired, because `enqueue` replaces the entry and resets
                // `attempts` -- so while a title played and sampled every
                // fifteen seconds, MAX_ATTEMPTS was reset before it could
                // count. `Dropped` existed for exactly this and had no
                // producer.
                if !mapped.is_retryable() {
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
    /// Returns [`BeamError::Storage`] when the queue cannot be read or written.
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
                    let _ = self.map_error(&server_id, &failure);
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
        let response = TransportFailure::capture(client.list_sessions(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        TransportFailure::capture(client.delete_session(session_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        TransportFailure::capture(client.logout_all(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;

        self.with_context(&server_id, |context| context.cookie.clear())?;
        self.storage
            .remove_secret(format!("session/{server_id}"))
            .await?;
        self.apply(&server_id, SessionEvent::LogoutRequested)?;
        Ok(())
    }

    // -- administration ---------------------------------------------------

    /// The admin dashboard snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn admin_status(&self) -> Result<AdminStatus, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = TransportFailure::capture(client.get_admin_status(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.list_admin_users(params))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?
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
        TransportFailure::capture(client.update_admin_user(wire_user_id, None, &body))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_admin_logs(params))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_admin_log_count(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_admin_events(params))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.create_library(None, &body))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
        Ok(LibrarySummary::from_generated(response.into_inner()))
    }

    /// Delete a library and everything indexed into it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn delete_library(&self, library_id: String) -> Result<(), BeamError> {
        let (server_id, client, _) = self.active_context()?;
        TransportFailure::capture(client.delete_library(library_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
        Ok(())
    }

    /// Rescan a library, returning how many files were added.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::Forbidden`] for a non-administrator.
    pub async fn scan_library(&self, library_id: String) -> Result<u32, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = TransportFailure::capture(client.scan_library(library_id, None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        TransportFailure::capture(client.refresh_media_metadata(media_id.clone(), None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
        let response = TransportFailure::capture(client.get_health(None))
            .await
            .map_err(|failure| self.map_error(&server_id, &failure))?;
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
    fn install(&self, record: ServerRecord, cookie: Option<&str>) -> Result<(), BeamError> {
        let holder = Arc::new(SessionCookieHolder::new());
        if let Some(value) = cookie {
            holder.set(value);
        }
        let middleware = Arc::new(SessionMiddleware::new(Arc::clone(&holder)));

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
        let client = Self::client_over(
            Arc::new(ReqwestBackend::new(http)),
            &middleware,
            &record.base_url,
        )?;

        // A restored cookie is trusted until a request says otherwise, which
        // is what keeps a cold start off the network.
        let state = if cookie.is_some() {
            SessionState::Expired
        } else {
            SessionState::LoggedOut
        };

        let queue = Arc::new(ProgressQueue::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.clock),
            &record.id,
        ));
        self.servers.write().expect("servers lock").insert(
            record.id.clone(),
            ServerContext {
                record,
                client,
                cookie: holder,
                middleware,
                state,
                throttle: Arc::new(ProgressThrottle::new(Arc::clone(&self.clock))),
                queue,
                metadata: Arc::new(RwLock::new(HashMap::new())),
                trust,
            },
        );
        Ok(())
    }

    /// The generated client, with the session middleware wrapped around
    /// whatever transport actually executes the request.
    fn client_over(
        transport: Arc<dyn crate::api::HttpBackend>,
        middleware: &Arc<SessionMiddleware>,
        base_url: &str,
    ) -> Result<GeneratedClient, BeamError> {
        let backend = MiddlewareBackend::with_middlewares(
            transport,
            vec![Arc::clone(middleware) as Arc<dyn crate::api::Middleware>],
        );
        GeneratedClient::with_backend(Arc::new(backend), base_url).map_err(|error| {
            BeamError::InvalidServerUrl {
                detail: error.to_string(),
            }
        })
    }

    /// Re-point one server's client at a canned transport, keeping its
    /// middleware, session and queue. Tests only: the seam that lets the façade
    /// see a server's answer without a listener.
    #[cfg(test)]
    fn use_transport(
        &self,
        server_id: &str,
        transport: Arc<dyn crate::api::HttpBackend>,
    ) -> Result<(), BeamError> {
        let mut servers = self.servers.write().expect("servers lock");
        let context = servers
            .get_mut(server_id)
            .ok_or_else(|| BeamError::UnknownServer {
                server_id: server_id.to_owned(),
            })?;
        // The generated client enforces each operation's `BeamSession`
        // requirement at construction time and refuses to send without a
        // registered credential; nothing in the core registers one, so a
        // canned answer would never be reached. A placeholder here lets the
        // request through to the transport under test. The middleware still
        // owns the real cookie: its `insert` replaces this header. Pre-existing
        // and unrelated to what these tests assert -- see the PR discussion.
        context.client =
            Self::client_over(transport, &context.middleware, &context.record.base_url)?
                .with_credential(
                    "BeamSession",
                    crate::api::Credential::ApiKey(secrecy::SecretString::from(
                        "test-placeholder".to_owned(),
                    )),
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
    fn map_error(&self, server_id: &str, failure: &TransportFailure) -> BeamError {
        let saw_401 = self
            .with_context(server_id, |context| context.middleware.take_unauthorized())
            .unwrap_or(false);
        if saw_401 {
            let _ = self.apply(server_id, SessionEvent::UnauthorizedObserved);
            return BeamError::SessionExpired;
        }
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
        }
    }

    async fn fetch_me(&self, server_id: &str) -> Result<UserSummary, BeamError> {
        let client = self.client_for(server_id)?;
        let response = TransportFailure::capture(client.get_current_user(None))
            .await
            .map_err(|failure| self.map_error(server_id, &failure))?;
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

    /// A server that answers every request with one canned response.
    async fn client_answering(status: u16, body: &'static str) -> (Arc<BeamClient>, String) {
        let (client, id) = client_with_server().await;
        client
            .use_transport(
                &id,
                Arc::new(crate::transport::CannedBackend {
                    status,
                    content_type: "application/problem+json",
                    body,
                }),
            )
            .expect("the server is registered");
        (client, id)
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

    /// The three shapes a failure can take, with and without a document.
    ///
    /// Only an answered request has a document to classify from; the other two
    /// never saw a response, so a document on them would be a bug upstream and
    /// is ignored rather than trusted.
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
                client.map_error(&id, &failure(FailureKind::Malformed, problem)),
                BeamError::Protocol {
                    detail: "the transport's own words".to_owned(),
                }
            );
        }
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
