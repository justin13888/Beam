//! Normalising codec names from two directions onto one vocabulary.
//!
//! Beam probes media with FFmpeg at index time and reports whatever name
//! FFmpeg used, as a free-form string (`MediaSource.video.codec`). Android
//! reports what it can decode as MIME types from `MediaCodecList`. Neither
//! vocabulary is the other's, and neither is stable enough to compare by
//! string equality, so both are normalised onto the enums here before any
//! capability decision is made.
//!
//! This is knowingly a duplicate of the smaller table in
//! `beam-server/src/models/media/codec.rs`. That one maps only the codecs the
//! *API* has enum variants for and collapses everything else to `UNKNOWN`; a
//! client that did the same would refuse to play AC-3 audio, which the vast
//! majority of Android devices decode natively. The client needs the wider
//! vocabulary, and it must not depend on `beam-domain` to get it.

/// A video codec, normalised from either vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
    /// AOMedia Video 1.
    Av1,
    /// VP9.
    Vp9,
    /// VP8.
    Vp8,
    /// MPEG-2 Part 2.
    Mpeg2,
    /// MPEG-4 Part 2.
    Mpeg4,
    /// SMPTE VC-1.
    Vc1,
    /// Recognised by neither table.
    Unknown,
}

/// An audio codec, normalised from either vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AudioCodec {
    /// Advanced Audio Coding.
    Aac,
    /// Opus.
    Opus,
    /// Dolby Digital.
    Ac3,
    /// Dolby Digital Plus.
    Eac3,
    /// DTS, including its extensions.
    Dts,
    /// Dolby TrueHD.
    TrueHd,
    /// Free Lossless Audio Codec.
    Flac,
    /// Vorbis.
    Vorbis,
    /// MPEG-1/2 Audio Layer III.
    Mp3,
    /// Linear PCM, in any of its endian/width spellings.
    Pcm,
    /// Recognised by neither table.
    Unknown,
}

impl VideoCodec {
    /// Normalise a codec name as FFmpeg probing reports it.
    #[must_use]
    pub fn from_probe(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "h264" | "avc" | "avc1" | "x264" => Self::H264,
            "h265" | "hevc" | "hvc1" | "hev1" | "x265" => Self::H265,
            "av1" | "av01" | "libaom-av1" | "libdav1d" => Self::Av1,
            "vp9" | "vp09" => Self::Vp9,
            "vp8" => Self::Vp8,
            "mpeg2video" | "mpeg2" => Self::Mpeg2,
            "mpeg4" | "msmpeg4v3" | "divx" | "xvid" => Self::Mpeg4,
            "vc1" | "vc-1" | "wmv3" => Self::Vc1,
            _ => Self::Unknown,
        }
    }

    /// Normalise an Android `MediaCodecList` MIME type.
    #[must_use]
    pub fn from_android_mime(mime: &str) -> Self {
        match mime.trim().to_ascii_lowercase().as_str() {
            "video/avc" => Self::H264,
            "video/hevc" => Self::H265,
            "video/av01" => Self::Av1,
            "video/x-vnd.on2.vp9" => Self::Vp9,
            "video/x-vnd.on2.vp8" => Self::Vp8,
            "video/mpeg2" => Self::Mpeg2,
            "video/mp4v-es" => Self::Mpeg4,
            "video/wvc1" | "video/vc1" => Self::Vc1,
            _ => Self::Unknown,
        }
    }
}

impl AudioCodec {
    /// Normalise a codec name as FFmpeg probing reports it.
    #[must_use]
    pub fn from_probe(name: &str) -> Self {
        let name = name.trim().to_ascii_lowercase();
        if name.starts_with("pcm_") {
            return Self::Pcm;
        }
        match name.as_str() {
            "aac" | "aac_latm" | "mp4a" => Self::Aac,
            "opus" | "libopus" => Self::Opus,
            "ac3" | "ac-3" => Self::Ac3,
            "eac3" | "e-ac-3" | "ec-3" => Self::Eac3,
            "dts" | "dca" | "dts-hd" | "dtshd" => Self::Dts,
            "truehd" | "mlp" => Self::TrueHd,
            "flac" => Self::Flac,
            "vorbis" | "libvorbis" => Self::Vorbis,
            "mp3" | "mp3float" => Self::Mp3,
            _ => Self::Unknown,
        }
    }

    /// Normalise an Android `MediaCodecList` MIME type.
    #[must_use]
    pub fn from_android_mime(mime: &str) -> Self {
        match mime.trim().to_ascii_lowercase().as_str() {
            "audio/mp4a-latm" => Self::Aac,
            "audio/opus" => Self::Opus,
            "audio/ac3" => Self::Ac3,
            "audio/eac3" | "audio/eac3-joc" => Self::Eac3,
            "audio/vnd.dts" | "audio/vnd.dts.hd" => Self::Dts,
            "audio/true-hd" => Self::TrueHd,
            "audio/flac" => Self::Flac,
            "audio/vorbis" => Self::Vorbis,
            "audio/mpeg" | "audio/mpeg-l3" => Self::Mp3,
            "audio/raw" => Self::Pcm,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_and_android_names_meet_on_the_same_video_codec() {
        // The whole point of the two tables: FFmpeg's name and Android's MIME
        // for one codec must normalise to one value, or nothing matches.
        for (probe, mime, expected) in [
            ("h264", "video/avc", VideoCodec::H264),
            ("hevc", "video/hevc", VideoCodec::H265),
            ("av1", "video/av01", VideoCodec::Av1),
            ("vp9", "video/x-vnd.on2.vp9", VideoCodec::Vp9),
        ] {
            assert_eq!(VideoCodec::from_probe(probe), expected);
            assert_eq!(VideoCodec::from_android_mime(mime), expected);
        }
    }

    #[test]
    fn probe_and_android_names_meet_on_the_same_audio_codec() {
        for (probe, mime, expected) in [
            ("aac", "audio/mp4a-latm", AudioCodec::Aac),
            ("opus", "audio/opus", AudioCodec::Opus),
            ("ac3", "audio/ac3", AudioCodec::Ac3),
            ("eac3", "audio/eac3", AudioCodec::Eac3),
            ("truehd", "audio/true-hd", AudioCodec::TrueHd),
            ("flac", "audio/flac", AudioCodec::Flac),
        ] {
            assert_eq!(AudioCodec::from_probe(probe), expected);
            assert_eq!(AudioCodec::from_android_mime(mime), expected);
        }
    }

    #[test]
    fn hevc_container_tags_normalise_to_h265() {
        // hvc1/hev1 are the MP4 sample-entry spellings; a library full of
        // Apple-authored files reports these rather than "hevc".
        for name in ["hevc", "hvc1", "hev1", "h265", "H265"] {
            assert_eq!(VideoCodec::from_probe(name), VideoCodec::H265);
        }
    }

    #[test]
    fn every_pcm_variant_normalises_to_pcm() {
        // FFmpeg spells PCM with an endianness/width suffix, so an exact-match
        // table would miss all of them.
        for name in ["pcm_s16le", "pcm_s24be", "pcm_f32le"] {
            assert_eq!(AudioCodec::from_probe(name), AudioCodec::Pcm);
        }
    }

    #[test]
    fn normalisation_ignores_case_and_surrounding_space() {
        assert_eq!(VideoCodec::from_probe("  HEVC "), VideoCodec::H265);
        assert_eq!(AudioCodec::from_probe("AAC"), AudioCodec::Aac);
        assert_eq!(
            VideoCodec::from_android_mime(" VIDEO/AVC "),
            VideoCodec::H264
        );
    }

    #[test]
    fn an_unrecognised_name_is_unknown_rather_than_a_wrong_guess() {
        assert_eq!(VideoCodec::from_probe("prores"), VideoCodec::Unknown);
        assert_eq!(AudioCodec::from_probe("alac"), AudioCodec::Unknown);
        assert_eq!(
            VideoCodec::from_android_mime("video/nonsense"),
            VideoCodec::Unknown
        );
    }
}
