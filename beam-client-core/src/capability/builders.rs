//! Test-data builders for device profiles and media sources.
//!
//! The repository's testing guidance asks for builders so a test states only
//! the property under test and inherits a valid remainder. These are shared
//! with the Android side through the `test-utils` feature, so a Kotlin-side
//! integration test can assert against the same fixtures.

use super::{DecoderCapability, DeviceProfile};
use crate::capability::select::{AudioTrackView, MediaSourceView};

/// Builds a [`DeviceProfile`] a test can reason about.
#[derive(Debug, Clone)]
pub struct DeviceProfileBuilder {
    profile: DeviceProfile,
}

impl Default for DeviceProfileBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProfileBuilder {
    /// A profile that decodes nothing, on a 1080p SDR screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: DeviceProfile {
                video_decoders: Vec::new(),
                audio_decoders: Vec::new(),
                supported_containers: vec!["mp4".to_owned(), "mkv".to_owned()],
                display_width: 1920,
                display_height: 1080,
                display_supports_hdr: false,
                preferred_audio_languages: Vec::new(),
                allow_software_decode: false,
            },
        }
    }

    /// A mid-range phone: hardware H.264 and HEVC to 4K, AAC and Opus, 1080p
    /// SDR screen. No AV1 -- which is exactly the interesting case.
    #[must_use]
    pub fn phone_h264_hevc() -> Self {
        Self::new()
            .hardware_video("video/avc", 3840, 2160)
            .hardware_video("video/hevc", 3840, 2160)
            .hardware_audio("audio/mp4a-latm")
            .hardware_audio("audio/opus")
            .hardware_audio("audio/ac3")
    }

    /// A budget device: hardware H.264 to 1080p and AAC only.
    #[must_use]
    pub fn budget_h264_only() -> Self {
        Self::new()
            .hardware_video("video/avc", 1920, 1080)
            .hardware_audio("audio/mp4a-latm")
    }

    /// A flagship: adds AV1 and HDR10 to [`Self::phone_h264_hevc`], on a 4K
    /// HDR screen.
    #[must_use]
    pub fn flagship_av1_hdr() -> Self {
        let mut builder = Self::phone_h264_hevc()
            .hardware_video("video/av01", 3840, 2160)
            .hardware_audio("audio/eac3");
        for decoder in &mut builder.profile.video_decoders {
            decoder.supports_hdr10 = true;
            decoder.supports_10_bit = true;
        }
        builder.profile.display_width = 3840;
        builder.profile.display_height = 2160;
        builder.profile.display_supports_hdr = true;
        builder
    }

    /// Add a hardware video decoder with the given ceiling.
    #[must_use]
    pub fn hardware_video(mut self, mime: &str, max_width: u32, max_height: u32) -> Self {
        self.profile.video_decoders.push(DecoderCapability {
            mime_type: mime.to_owned(),
            is_hardware_accelerated: true,
            max_width: Some(max_width),
            max_height: Some(max_height),
            max_bitrate_bps: None,
            supports_hdr10: false,
            supports_dolby_vision: false,
            supports_10_bit: false,
        });
        self
    }

    /// Add a software video decoder with the given ceiling.
    #[must_use]
    pub fn software_video(mut self, mime: &str, max_width: u32, max_height: u32) -> Self {
        self.profile.video_decoders.push(DecoderCapability {
            mime_type: mime.to_owned(),
            is_hardware_accelerated: false,
            max_width: Some(max_width),
            max_height: Some(max_height),
            max_bitrate_bps: None,
            supports_hdr10: false,
            supports_dolby_vision: false,
            supports_10_bit: false,
        });
        self
    }

    /// Add a hardware audio decoder.
    #[must_use]
    pub fn hardware_audio(mut self, mime: &str) -> Self {
        self.profile.audio_decoders.push(DecoderCapability {
            mime_type: mime.to_owned(),
            is_hardware_accelerated: true,
            max_width: None,
            max_height: None,
            max_bitrate_bps: None,
            supports_hdr10: false,
            supports_dolby_vision: false,
            supports_10_bit: false,
        });
        self
    }

    /// Replace the demuxable container list.
    #[must_use]
    pub fn containers(mut self, containers: &[&str]) -> Self {
        self.profile.supported_containers = containers.iter().map(|c| (*c).to_owned()).collect();
        self
    }

    /// Set the display resolution.
    #[must_use]
    pub fn display(mut self, width: u32, height: u32) -> Self {
        self.profile.display_width = width;
        self.profile.display_height = height;
        self
    }

    /// Allow software decoders to satisfy a source.
    #[must_use]
    pub fn allow_software(mut self) -> Self {
        self.profile.allow_software_decode = true;
        self
    }

    /// Set the preferred audio languages, best first.
    #[must_use]
    pub fn preferred_languages(mut self, languages: &[&str]) -> Self {
        self.profile.preferred_audio_languages =
            languages.iter().map(|l| (*l).to_owned()).collect();
        self
    }

    /// Finish.
    #[must_use]
    pub fn build(self) -> DeviceProfile {
        self.profile
    }
}

/// Builds a [`MediaSourceView`] a test can reason about.
#[derive(Debug, Clone)]
pub struct MediaSourceBuilder {
    source: MediaSourceView,
}

impl MediaSourceBuilder {
    /// A 1080p H.264 + AAC MP4, the least surprising thing a library holds.
    #[must_use]
    pub fn new(file_id: &str) -> Self {
        Self {
            source: MediaSourceView {
                file_id: file_id.to_owned(),
                size_bytes: 4_000_000_000,
                duration_secs: Some(7200.0),
                container: Some("mp4".to_owned()),
                mime_type: Some("video/mp4".to_owned()),
                video_codec: Some("h264".to_owned()),
                width: Some(1920),
                height: Some(1080),
                bit_rate: Some(8_000_000),
                hdr_format: None,
                audio_tracks: vec![AudioTrackView {
                    codec: "aac".to_owned(),
                    language: Some("eng".to_owned()),
                    channels: 2,
                    is_default: true,
                }],
                stream_url: format!("/v1/files/{file_id}/stream"),
                download_url: format!("/v1/files/{file_id}/download"),
            },
        }
    }

    /// Set the video stream's codec and geometry.
    #[must_use]
    pub fn video(mut self, codec: &str, width: u32, height: u32) -> Self {
        self.source.video_codec = Some(codec.to_owned());
        self.source.width = Some(width);
        self.source.height = Some(height);
        self
    }

    /// Set the overall bit rate.
    #[must_use]
    pub fn bitrate(mut self, bits_per_second: u64) -> Self {
        self.source.bit_rate = Some(bits_per_second);
        self
    }

    /// Set the container format.
    #[must_use]
    pub fn container(mut self, container: &str) -> Self {
        self.source.container = Some(container.to_owned());
        self
    }

    /// Mark the video stream as HDR of the given format.
    #[must_use]
    pub fn hdr(mut self, format: &str) -> Self {
        self.source.hdr_format = Some(format.to_owned());
        self
    }

    /// Set the file size.
    #[must_use]
    pub fn size(mut self, bytes: u64) -> Self {
        self.source.size_bytes = bytes;
        self
    }

    /// Replace the audio tracks.
    #[must_use]
    pub fn audio(mut self, tracks: &[(&str, Option<&str>, u16, bool)]) -> Self {
        self.source.audio_tracks = tracks
            .iter()
            .map(|(codec, language, channels, is_default)| AudioTrackView {
                codec: (*codec).to_owned(),
                language: language.map(str::to_owned),
                channels: *channels,
                is_default: *is_default,
            })
            .collect();
        self
    }

    /// Remove the video stream entirely.
    #[must_use]
    pub fn without_video(mut self) -> Self {
        self.source.video_codec = None;
        self.source.width = None;
        self.source.height = None;
        self
    }

    /// Finish.
    #[must_use]
    pub fn build(self) -> MediaSourceView {
        self.source
    }
}
