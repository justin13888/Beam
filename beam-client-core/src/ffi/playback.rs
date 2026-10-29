//! What the platform player is handed: the sources a title can play, the one
//! chosen for this device, and the URL, headers and trust decision it streams
//! with.

use super::{BeamClient, parse_uuid};
use crate::capability::{
    DeviceProfile, MediaSourceView, QualityPolicy, RebufferDetector, SourceSelection,
};
use crate::error::BeamError;
use crate::transport::ABOUT_BLANK;
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How long [`BeamClient::playback_plan`] waits for a title's sources before
/// it plays the file alone.
///
/// The lookup runs before every start, a downloaded file's included, and the
/// shared HTTP client sets no timeout of its own: on a network that drops
/// packets, an unbounded lookup would hold a download that plays offline for
/// as long as the OS takes to give up on the connection. Three seconds is
/// several round trips on any connection that could stream, and short enough
/// that a start without one still feels like a start.
pub(super) const PLAYBACK_PLAN_LOOKUP_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(3);

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

/// Everything the platform player needs to play one source, part by part.
///
/// A multi-part movie (issue #233) is one source whose parts the player queues
/// as consecutive items; a whole file is a plan of one part. Every part is a
/// direct play of one file -- the server never joins them (ADR-0004) -- and
/// progress is reported against the part that is playing, at the position
/// within it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlaybackPlan {
    /// The files to queue, in order. Never empty.
    pub parts: Vec<crate::capability::select::PlaybackPart>,
    /// Which part to start in: the one the request named, which for a resume
    /// is the part the viewer was last in.
    pub start_part_index: u32,
    /// Headers every part's request carries, including the session cookie.
    pub headers: HashMap<String, String>,
    /// Certificates the user has accepted for this server; see
    /// [`PlaybackHttpConfig::trusted_fingerprints`].
    pub trusted_fingerprints: Vec<String>,
    /// The host those fingerprints apply to.
    pub pinned_host: String,
    /// The server's normalised origin, which every part's URL is on. The
    /// player attaches [`Self::headers`] only to requests for this origin, so
    /// a redirect elsewhere loses the session at the hop that leaves.
    pub base_url: String,
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

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
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
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::BadRequest`] when `media_id` is not a UUID,
    /// [`BeamError::InvalidServerUrl`] when a source names a URL off the
    /// server's origin, and the [request failures](BeamClient#request-failures).
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
            .map(|source| MediaSourceView::from_generated(source, &record))
            .collect()
    }

    /// Choose a source for a title, given the device profile and a policy.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NotFound`] when nothing can play here, its detail
    /// the first rejected source's reason, and [`BeamError::BadRequest`]
    /// before the device profile has been set. Otherwise it fails as
    /// [`Self::media_sources`] does.
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
    /// Returns [`BeamError::BadRequest`] before the device profile has been
    /// set. Otherwise it fails as [`Self::media_sources`] does.
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

    /// Everything the platform player needs to stream a file itself.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// [`BeamError::Unauthenticated`] when there is no session, and
    /// [`BeamError::InvalidServerUrl`] when the stream URL cannot be built on
    /// the server's origin.
    pub fn playback_config(&self, file_id: String) -> Result<PlaybackHttpConfig, BeamError> {
        let server = self.server_http_config()?;
        let ServerHttpConfig {
            base_url: _,
            headers,
            trusted_fingerprints,
            host,
        } = server;

        // Built by hand, because the generated client has no way to name an
        // operation's URL without sending it, and the platform player sends
        // this one itself (getkono/spargen#1067). Where the title's sources
        // are known, `playback_plan` takes each part's `stream_url` from them
        // instead.
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

    /// Everything the platform player needs to play `file_id` with the rest
    /// of its source: every part, in order, and which to start in.
    ///
    /// `title_id` is the movie or episode the file belongs to, which is where
    /// its source -- and so its parts -- are listed. Without one, or when the
    /// lookup fails or takes longer than three seconds,
    /// the plan is the file alone: a downloaded file played
    /// with no connection is exactly that, and a failed lookup must not stop
    /// a file that would play from playing.
    ///
    /// # Errors
    ///
    /// As [`Self::playback_config`]. A failed or slow source lookup is not an
    /// error: the plan is then the file alone.
    pub async fn playback_plan(
        &self,
        title_id: Option<String>,
        file_id: String,
    ) -> Result<PlaybackPlan, BeamError> {
        let PlaybackHttpConfig {
            url,
            headers,
            trusted_fingerprints,
            pinned_host,
        } = self.playback_config(file_id.clone())?;
        let base_url = self.server_http_config()?.base_url;

        let sources = match title_id {
            Some(title_id) => {
                let lookup = self.media_sources(title_id);
                match tokio::time::timeout(PLAYBACK_PLAN_LOOKUP_TIMEOUT, lookup).await {
                    Ok(Ok(sources)) => sources,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "playing one file alone: its source could not be listed");
                        Vec::new()
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = PLAYBACK_PLAN_LOOKUP_TIMEOUT.as_secs(),
                            "playing one file alone: listing its source timed out"
                        );
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };
        let (parts, start_part_index) =
            crate::capability::select::plan_parts(&sources, &file_id, url);
        Ok(PlaybackPlan {
            parts,
            start_part_index,
            headers,
            trusted_fingerprints,
            pinned_host,
            base_url,
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
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// [`BeamError::Unauthenticated`] when there is no session.
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
}

impl BeamClient {
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
}
