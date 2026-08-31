use kynos::Schema;
use rust_decimal::Decimal;
use serde::Serialize;

use super::{OutputAudioCodec, OutputSubtitleCodec, OutputVideoCodec, Resolution};

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct MediaStreamMetadata {
    /// Video tracks
    pub video_tracks: Vec<VideoTrack>,
    /// Audio tracks
    pub audio_tracks: Vec<AudioTrack>,
    /// Subtitle tracks
    pub subtitle_tracks: Vec<SubtitleTrack>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct VideoTrack {
    /// The target video codec
    pub codec: OutputVideoCodec,

    /// Maximum bitrate (in bits per second)
    pub max_rate: u64,

    /// Average bitrate (in bits per second)
    pub bit_rate: u64,

    /// Resolution
    pub resolution: Resolution,

    /// Frame rate (frames per second)
    pub frame_rate: Decimal,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct AudioTrack {
    /// The target audio codec (e.g., "aac", "opus", "ac3").
    pub codec: OutputAudioCodec,

    /// The ISO 639-2/B 3-letter language code (e.g., "eng", "jpn").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// A descriptive title for the audio track (e.g., "English", "日本語").
    pub title: String,

    /// Channel layout description (e.g., "stereo", "5.1").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,

    /// Default means client should select this track if no other preference is given.
    pub is_default: bool,

    /// Autoselect means client may automatically choose, typically based on user preferences (e.g. system language).
    pub is_autoselect: bool,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SubtitleTrack {
    /// The target subtitle format.
    pub codec: OutputSubtitleCodec,

    /// The ISO 639-2/B 3-letter language code (e.g., "eng", "spa").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// A descriptive title for the subtitle track (e.g., "SDH", "Commentary").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Default means client should select this track if no other preference is given.
    pub is_default: bool,

    /// Autoselect means client may automatically choose, typically based on user preferences (e.g. system language).
    pub is_autoselect: bool,

    /// Flag indicating if this is a "forced" subtitle track (e.g., for foreign audio only).
    pub is_forced: bool,
}
