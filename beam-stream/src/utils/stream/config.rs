use crate::utils::{
    codec::{OutputAudioCodec, OutputSubtitleCodec, OutputVideoCodec},
    file::FileType,
    format::Resolution,
    hash::XXH3Hash,
    metadata::Rational,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// StreamConfiguration struct allows HLS and DASH streams to be constructed
/// determinsitically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfiguration {
    /// List of all input sources to construct a stream.
    pub sources: Vec<(FileType, PathBuf, XXH3Hash)>, // TODO: Refactor this tuple into its own struct
    /// A collection of all output streams in the final media
    pub streams: Vec<OutputStream>,
    /// Target segment duration in seconds
    pub target_duration: u64,
}

impl StreamConfiguration {
    /// Get all video streams
    pub fn video_streams(&self) -> Vec<&VideoStream> {
        self.streams
            .iter()
            .filter_map(|s| match s {
                OutputStream::Video(vs) => Some(vs),
                _ => None,
            })
            .collect()
    }

    /// Get all audio streams
    pub fn audio_streams(&self) -> Vec<&AudioStream> {
        self.streams
            .iter()
            .filter_map(|s| match s {
                OutputStream::Audio(as_) => Some(as_),
                _ => None,
            })
            .collect()
    }

    /// Get all subtitle streams
    pub fn subtitle_streams(&self) -> Vec<&SubtitleStream> {
        self.streams
            .iter()
            .filter_map(|s| match s {
                OutputStream::Subtitle(ss) => Some(ss),
                _ => None,
            })
            .collect()
    }
}

/// Different streams
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputStream {
    Video(VideoStream),
    Audio(AudioStream),
    Subtitle(SubtitleStream),
}

/// Output video stream configuration. Video streams have no audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStream {
    /// The index of the source file in the `StreamConfiguration::sources` vector.
    pub source_file_index: usize,

    /// The index of the video stream within the specified source file (e.g., 0 for the first video stream).
    pub source_stream_index: usize,

    /// The target video codec
    pub codec: OutputVideoCodec,

    /// Maximum bitrate (in bits per second)
    pub max_rate: u64,

    /// Average bitrate (in bits per second)
    pub bit_rate: u64,

    /// Resolution
    pub resolution: Resolution,

    /// Frame rate (frames per second)
    pub frame_rate: Rational,
}

/// Audio stream configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStream {
    /// The index of the source file in the `StreamConfiguration::sources` vector.
    pub source_file_index: usize,

    /// The index of the audio stream within the specified source file.
    pub source_stream_index: usize,

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
    // /// Optional target bitrate in kilobits per second (kbps).
    // #[serde(default, skip_serializing_if = "Option::is_none")]
    // pub bitrate_kbps: Option<u32>,
}

/// Defines the properties for an output subtitle stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleStream {
    /// The index of the source file in the `StreamConfiguration::sources` vector.
    /// This could be a video file with an embedded subtitle or a standalone subtitle file (e.g., `.srt`).
    pub source_file_index: usize,

    /// The index of the subtitle stream within the source file.
    /// If the source is a standalone subtitle file, this would typically be 0.
    pub source_stream_index: usize,

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
