use salvo::oapi::ToSchema;
use serde::Serialize;

/// One playable/downloadable version of a media item. Beam models multiple
/// deliverable qualities/editions as distinct source files rather than
/// transcoding on demand (see ADR-0004: never live-transcode); this is how a
/// client picks among them for constrained-bandwidth playback.
#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct MediaSource {
    pub file_id: String,
    pub size_bytes: u64,
    pub mime_type: Option<String>,
    pub container_format: Option<String>,
    pub duration_secs: Option<f64>,
    pub video: Option<VideoSourceInfo>,
    pub audio_tracks: Vec<AudioSourceInfo>,
    /// Direct-play stream URL for this file (Range-request capable).
    pub stream_url: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct VideoSourceInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub bit_rate: Option<u64>,
    pub hdr_format: Option<String>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct AudioSourceInfo {
    pub codec: String,
    pub language: Option<String>,
    pub channels: u16,
    pub is_default: bool,
}
