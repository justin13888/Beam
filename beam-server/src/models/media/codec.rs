use salvo::oapi::ToSchema;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
pub enum OutputVideoCodec {
    H264,
    H265,
    AV1,
    UNKNOWN,
}

impl OutputVideoCodec {
    /// Maps an ffmpeg-probed codec name (e.g. `h264`, `hevc`, `av1`) to the
    /// API-visible codec. Beam never transcodes (ADR-0004), so this reports
    /// the source stream's codec as-is; anything unrecognized is `UNKNOWN`.
    pub fn from_probe_str(codec: &str) -> Self {
        match codec.to_ascii_lowercase().as_str() {
            "h264" | "avc" | "avc1" => OutputVideoCodec::H264,
            "h265" | "hevc" | "hvc1" | "hev1" => OutputVideoCodec::H265,
            "av1" => OutputVideoCodec::AV1,
            _ => OutputVideoCodec::UNKNOWN,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
pub enum OutputAudioCodec {
    Aac,
    Opus,
    Unknown,
}

impl OutputAudioCodec {
    /// Maps an ffmpeg-probed codec name (e.g. `aac`, `opus`) to the
    /// API-visible codec; anything unrecognized is `Unknown`.
    pub fn from_probe_str(codec: &str) -> Self {
        match codec.to_ascii_lowercase().as_str() {
            "aac" => OutputAudioCodec::Aac,
            "opus" => OutputAudioCodec::Opus,
            _ => OutputAudioCodec::Unknown,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_from_probe_str_maps_known_names_case_insensitively() {
        let cases = [
            ("h264", OutputVideoCodec::H264),
            ("H264", OutputVideoCodec::H264),
            ("avc1", OutputVideoCodec::H264),
            ("hevc", OutputVideoCodec::H265),
            ("HEVC", OutputVideoCodec::H265),
            ("h265", OutputVideoCodec::H265),
            ("av1", OutputVideoCodec::AV1),
            ("vp9", OutputVideoCodec::UNKNOWN),
            ("mpeg2video", OutputVideoCodec::UNKNOWN),
            ("", OutputVideoCodec::UNKNOWN),
        ];
        for (probed, expected) in cases {
            assert_eq!(
                OutputVideoCodec::from_probe_str(probed),
                expected,
                "probed codec: {probed:?}"
            );
        }
    }

    #[test]
    fn audio_from_probe_str_maps_known_names_case_insensitively() {
        let cases = [
            ("aac", OutputAudioCodec::Aac),
            ("AAC", OutputAudioCodec::Aac),
            ("opus", OutputAudioCodec::Opus),
            ("Opus", OutputAudioCodec::Opus),
            ("ac3", OutputAudioCodec::Unknown),
            ("flac", OutputAudioCodec::Unknown),
            ("", OutputAudioCodec::Unknown),
        ];
        for (probed, expected) in cases {
            assert_eq!(
                OutputAudioCodec::from_probe_str(probed),
                expected,
                "probed codec: {probed:?}"
            );
        }
    }
}
