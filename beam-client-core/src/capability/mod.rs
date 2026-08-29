//! Deciding which of a title's files this device should actually play.
//!
//! Beam never transcodes ([ADR-0004]), so the server offers whatever files an
//! operator happened to index and the client must choose among them. That
//! makes this module the substance of a native client: a browser that cannot
//! decode HEVC simply fails, whereas a device with a hardware HEVC decoder
//! plays the same file untouched. The rule lives here, once, rather than
//! three times across Android, Apple, and GTK.
//!
//! Quality switching is a discrete user action, never an automatic ladder --
//! there is no ABR to fall back on, so a wrong automatic choice is a failed
//! playback rather than a rebuffer.
//!
//! [ADR-0004]: ../../../docs/architecture/decisions/ADR-0004-never-transcode.md

pub mod codec;
pub mod select;

pub use codec::{AudioCodec, VideoCodec};
pub use select::{
    MediaSourceView, Playability, QualityPolicy, RejectedSource, RejectionReason, SourceSelection,
    select_source,
};

/// One decoder the device reports through `MediaCodecList`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DecoderCapability {
    /// The MIME type, exactly as Android reported it.
    pub mime_type: String,

    /// Whether the decoder is hardware-backed. Software decoders are treated
    /// as a last resort rather than a disqualification: they play, but a 4K
    /// HEVC stream through one will stutter, and the user is told so.
    pub is_hardware_accelerated: bool,

    /// Largest width the decoder advertises, if it declared one.
    pub max_width: Option<u32>,

    /// Largest height the decoder advertises, if it declared one.
    pub max_height: Option<u32>,

    /// Highest bit rate the decoder advertises, if it declared one.
    pub max_bitrate_bps: Option<u64>,

    /// Whether the decoder advertises an HDR10 profile.
    pub supports_hdr10: bool,

    /// Whether the decoder advertises a Dolby Vision profile.
    pub supports_dolby_vision: bool,

    /// Whether the decoder advertises a 10-bit profile. An 8-bit-only decoder
    /// cannot play a 10-bit stream even at a supported resolution.
    pub supports_10_bit: bool,
}

impl DecoderCapability {
    /// Whether this decoder can carry `width` x `height`.
    ///
    /// A decoder that declared no maximum is taken at its word rather than
    /// assumed unlimited-but-suspect: `MediaCodecInfo` omitting the field
    /// means "no declared ceiling", and inventing one here would reject
    /// sources that in fact play.
    #[must_use]
    pub fn accepts_size(&self, width: u32, height: u32) -> bool {
        self.max_width.is_none_or(|max| width <= max)
            && self.max_height.is_none_or(|max| height <= max)
    }

    /// Whether this decoder can carry `bits_per_second`.
    #[must_use]
    pub fn accepts_bitrate(&self, bits_per_second: u64) -> bool {
        self.max_bitrate_bps
            .is_none_or(|max| bits_per_second <= max)
    }
}

/// What this device can decode and display.
///
/// Assembled on the foreign side, where the platform APIs live, and handed to
/// the core so the decision itself stays portable.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DeviceProfile {
    /// Every video decoder the device reports.
    pub video_decoders: Vec<DecoderCapability>,

    /// Every audio decoder the device reports.
    pub audio_decoders: Vec<DecoderCapability>,

    /// Containers the player's extractors can demux, lowercased (`mkv`,
    /// `mp4`, ...). Codec support is worthless if the container cannot be
    /// opened, and this is the most common reason a file that "should" play
    /// does not.
    pub supported_containers: Vec<String>,

    /// The display's width in pixels, used by
    /// [`QualityPolicy::MatchScreen`].
    pub display_width: u32,

    /// The display's height in pixels.
    pub display_height: u32,

    /// Whether the display can present HDR.
    pub display_supports_hdr: bool,

    /// Preferred audio languages, best first, as ISO 639 codes.
    pub preferred_audio_languages: Vec<String>,

    /// Whether to consider software decoders at all. Off by default on
    /// battery-powered devices in the UI, but the core does not assume.
    pub allow_software_decode: bool,
}

impl DeviceProfile {
    /// The best decoder for `codec`, preferring hardware.
    #[must_use]
    pub fn video_decoder_for(&self, codec: VideoCodec) -> Option<&DecoderCapability> {
        best_decoder(&self.video_decoders, |mime| {
            VideoCodec::from_android_mime(mime) == codec
        })
    }

    /// The best decoder for `codec`, preferring hardware.
    #[must_use]
    pub fn audio_decoder_for(&self, codec: AudioCodec) -> Option<&DecoderCapability> {
        best_decoder(&self.audio_decoders, |mime| {
            AudioCodec::from_android_mime(mime) == codec
        })
    }

    /// Whether the player can demux `container`.
    #[must_use]
    pub fn supports_container(&self, container: &str) -> bool {
        let container = container
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if container.is_empty() {
            // Beam does not always know the container. Refusing to play on
            // that basis would be worse than trying: the extractor sniffs
            // content anyway.
            return true;
        }
        self.supported_containers.iter().any(|known| {
            known
                .trim()
                .trim_start_matches('.')
                .eq_ignore_ascii_case(&container)
        })
    }
}

fn best_decoder(
    decoders: &[DecoderCapability],
    matches: impl Fn(&str) -> bool,
) -> Option<&DecoderCapability> {
    decoders
        .iter()
        .filter(|decoder| matches(&decoder.mime_type))
        // Hardware first, then the most capable of the remainder.
        .max_by_key(|decoder| {
            (
                decoder.is_hardware_accelerated,
                decoder.max_height.unwrap_or(u32::MAX),
            )
        })
}

#[cfg(any(test, feature = "test-utils"))]
pub mod builders;

#[cfg(test)]
mod tests {
    use super::builders::DeviceProfileBuilder;
    use super::*;

    #[test]
    fn a_decoder_with_no_declared_ceiling_accepts_anything() {
        let decoder = DecoderCapability {
            mime_type: "video/avc".to_owned(),
            is_hardware_accelerated: true,
            max_width: None,
            max_height: None,
            max_bitrate_bps: None,
            supports_hdr10: false,
            supports_dolby_vision: false,
            supports_10_bit: false,
        };
        assert!(decoder.accepts_size(7680, 4320));
        assert!(decoder.accepts_bitrate(u64::MAX));
    }

    #[test]
    fn a_declared_ceiling_is_enforced_on_both_axes() {
        let decoder = DecoderCapability {
            mime_type: "video/avc".to_owned(),
            is_hardware_accelerated: true,
            max_width: Some(1920),
            max_height: Some(1080),
            max_bitrate_bps: Some(20_000_000),
            supports_hdr10: false,
            supports_dolby_vision: false,
            supports_10_bit: false,
        };
        assert!(decoder.accepts_size(1920, 1080));
        assert!(!decoder.accepts_size(3840, 2160));
        assert!(decoder.accepts_bitrate(20_000_000));
        assert!(!decoder.accepts_bitrate(80_000_000));
    }

    #[test]
    fn a_hardware_decoder_is_chosen_over_a_software_one_for_the_same_codec() {
        let profile = DeviceProfileBuilder::new()
            .software_video("video/hevc", 3840, 2160)
            .hardware_video("video/hevc", 1920, 1080)
            .build();
        let chosen = profile
            .video_decoder_for(VideoCodec::H265)
            .expect("a decoder");
        assert!(
            chosen.is_hardware_accelerated,
            "hardware must win even when the software decoder claims more pixels"
        );
    }

    #[test]
    fn container_support_ignores_case_and_a_leading_dot() {
        let profile = DeviceProfileBuilder::new()
            .containers(&["mkv", "mp4"])
            .build();
        assert!(profile.supports_container("MKV"));
        assert!(profile.supports_container(".mp4"));
        assert!(!profile.supports_container("avi"));
    }

    #[test]
    fn an_unknown_container_is_attempted_rather_than_refused() {
        // Beam leaves container_format null for some files. Refusing on that
        // basis would reject sources that play fine, since the extractor
        // sniffs the content regardless.
        let profile = DeviceProfileBuilder::new().containers(&["mkv"]).build();
        assert!(profile.supports_container(""));
    }
}
