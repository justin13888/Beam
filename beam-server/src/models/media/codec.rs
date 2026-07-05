use async_graphql::Enum;
use salvo::oapi::ToSchema;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, Enum)]
pub enum OutputVideoCodec {
    H264,
    H265,
    AV1,
    UNKNOWN,
}

impl From<&crate::utils::codec::OutputVideoCodec> for OutputVideoCodec {
    fn from(codec: &crate::utils::codec::OutputVideoCodec) -> Self {
        match codec {
            crate::utils::codec::OutputVideoCodec::Remuxed(c) => {
                match c.to_ascii_lowercase().as_str() {
                    "h264" => OutputVideoCodec::H264,
                    "h265" => OutputVideoCodec::H265,
                    "av1" => OutputVideoCodec::AV1,
                    _ => OutputVideoCodec::UNKNOWN,
                }
            }
            crate::utils::codec::OutputVideoCodec::H264 => OutputVideoCodec::H264,
            crate::utils::codec::OutputVideoCodec::H265 => OutputVideoCodec::H265,
            crate::utils::codec::OutputVideoCodec::AV1 => OutputVideoCodec::AV1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, Enum)]
pub enum OutputAudioCodec {
    Aac,
    Opus,
    Unknown,
}

impl From<&crate::utils::codec::OutputAudioCodec> for OutputAudioCodec {
    fn from(codec: &crate::utils::codec::OutputAudioCodec) -> Self {
        match codec {
            crate::utils::codec::OutputAudioCodec::Remuxed(c) => {
                match c.to_ascii_lowercase().as_str() {
                    "aac" => OutputAudioCodec::Aac,
                    "opus" => OutputAudioCodec::Opus,
                    _ => OutputAudioCodec::Unknown,
                }
            }
            crate::utils::codec::OutputAudioCodec::AacLc => OutputAudioCodec::Aac,
            crate::utils::codec::OutputAudioCodec::Opus => OutputAudioCodec::Opus,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, Enum)]
pub enum OutputSubtitleCodec {
    WebVTT,
}

impl From<&crate::utils::codec::OutputSubtitleCodec> for OutputSubtitleCodec {
    fn from(codec: &crate::utils::codec::OutputSubtitleCodec) -> Self {
        match codec {
            crate::utils::codec::OutputSubtitleCodec::WebVTT => OutputSubtitleCodec::WebVTT,
        }
    }
}
