use std::collections::HashMap;

use crate::utils::stream::config::StreamConfiguration;
use async_graphql::SimpleObject;
use rust_decimal::Decimal;
use salvo::oapi::ToSchema;
use serde::Serialize;

use super::{OutputAudioCodec, OutputSubtitleCodec, OutputVideoCodec, Resolution};

#[derive(Clone, Debug, Serialize, ToSchema, SimpleObject)]
pub struct MediaStreamMetadata {
    /// Video tracks
    pub video_tracks: Vec<VideoTrack>,
    /// Audio tracks
    pub audio_tracks: Vec<AudioTrack>,
    /// Subtitle tracks
    pub subtitle_tracks: Vec<SubtitleTrack>,
}

impl From<&StreamConfiguration> for MediaStreamMetadata {
    fn from(config: &StreamConfiguration) -> Self {
        Self {
            video_tracks: config
                .video_streams()
                .iter()
                .map(|vs| VideoTrack {
                    codec: (&vs.codec).into(),
                    max_rate: vs.max_rate,
                    bit_rate: vs.bit_rate,
                    resolution: (&vs.resolution).into(),
                    frame_rate: {
                        let ratio = vs.frame_rate;
                        let n = *ratio.numer();
                        let d = *ratio.denom();
                        Decimal::from(n) / Decimal::from(d)
                    },
                })
                .collect(),
            audio_tracks: config
                .audio_streams()
                .iter()
                .map(|as_| AudioTrack {
                    codec: (&as_.codec).into(),
                    language: as_.language.clone(),
                    title: as_.title.clone(),
                    channel_layout: as_.channel_layout.clone(),
                    is_default: as_.is_default,
                    is_autoselect: as_.is_autoselect,
                })
                .collect(),
            subtitle_tracks: config
                .subtitle_streams()
                .iter()
                .map(|ss| SubtitleTrack {
                    codec: (&ss.codec).into(),
                    language: ss.language.clone(),
                    title: ss.title.clone(),
                    is_default: ss.is_default,
                    is_autoselect: ss.is_autoselect,
                    is_forced: ss.is_forced,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema, SimpleObject)]
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

#[derive(Clone, Debug, Serialize, ToSchema, SimpleObject)]
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

#[derive(Clone, Debug, Serialize, ToSchema, SimpleObject)]
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

/// Stream ID
pub type StreamID = String;

/// Mapping from stream IDs to their configurations
pub type StreamMapping = HashMap<StreamID, StreamConfiguration>;
