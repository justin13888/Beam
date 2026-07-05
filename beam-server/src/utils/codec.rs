use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputVideoCodec {
    /// Remuxed(codec_name) - `codec_name` could be `h264`, `h265`, `av1`, etc.
    Remuxed(String),
    H264,
    H265,
    AV1,
}

impl std::fmt::Display for OutputVideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputVideoCodec::Remuxed(name) => write!(f, "{name}"),
            OutputVideoCodec::H264 => write!(f, "h264"),
            OutputVideoCodec::H265 => write!(f, "h265"),
            OutputVideoCodec::AV1 => write!(f, "av1"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputAudioCodec {
    /// Remuxed(codec_name) - `codec_name` could be `aac`, `opus`, `ac3`, etc.
    Remuxed(String),
    AacLc,
    Opus,
}

impl std::fmt::Display for OutputAudioCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputAudioCodec::Remuxed(name) => write!(f, "{name}"),
            OutputAudioCodec::AacLc => write!(f, "aac"),
            OutputAudioCodec::Opus => write!(f, "opus"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSubtitleCodec {
    WebVTT,
}

impl OutputSubtitleCodec {
    pub fn file_extension(&self) -> &str {
        match self {
            OutputSubtitleCodec::WebVTT => "vtt",
        }
    }
}

impl std::fmt::Display for OutputSubtitleCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputSubtitleCodec::WebVTT => write!(f, "webvtt"),
        }
    }
}
