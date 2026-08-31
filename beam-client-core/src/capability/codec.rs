//! Normalising codec names from four directions onto one vocabulary.
//!
//! Beam probes media with FFmpeg at index time and reports whatever name
//! FFmpeg used, as a free-form string (`MediaSource.video.codec`). Android
//! reports what it can decode as MIME types from `MediaCodecList`. Apple
//! reports it as `CMVideoCodecType`/`AudioFormatID` four-character codes. And
//! the Matroska container names its own tracks in a fourth vocabulary again,
//! which the client's own demuxer reads (see `crate::demux`). None of the four
//! is the others', and none is stable enough to compare by string equality, so
//! all four are normalised onto the enums here before any capability decision
//! is made.
//!
//! The direction matters. Probe names and Matroska codec IDs describe *media*;
//! Android MIME types and Apple four-character codes describe *decoders*. A
//! capability decision is only ever a comparison between one of each, which is
//! why every table has to land on the same enum rather than on its own.
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

    /// Normalise an Apple `CMVideoCodecType`, as its four-character code.
    ///
    /// Swift renders the `OSType` as the four bytes it is rather than as the
    /// integer, because the integer is unreadable in a table and a mistyped
    /// digit would be invisible in review.
    #[must_use]
    pub fn from_apple_fourcc(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "avc1" => Self::H264,
            // Both HEVC sample entries. `hvc1` carries its parameter sets in
            // the sample description and `hev1` may carry them in-band; the
            // distinction matters when building a format description, not when
            // asking whether the device decodes HEVC at all.
            "hvc1" | "hev1" => Self::H265,
            "av01" => Self::Av1,
            "vp09" => Self::Vp9,
            "vp08" => Self::Vp8,
            "mp4v" => Self::Mpeg4,
            "mp2v" => Self::Mpeg2,
            _ => Self::Unknown,
        }
    }

    /// Normalise a Matroska `CodecID`, as the container spells it.
    ///
    /// Matched by prefix, because the profile suffix is part of the ID for
    /// some codecs (`V_MPEG4/ISO/AVC` against `V_MPEG4/ISO/SP`) and absent for
    /// others.
    #[must_use]
    pub fn from_matroska_codec_id(codec_id: &str) -> Self {
        let id = codec_id.trim().to_ascii_uppercase();
        if id.starts_with("V_MPEG4/ISO/AVC") {
            return Self::H264;
        }
        if id.starts_with("V_MPEGH/ISO/HEVC") {
            return Self::H265;
        }
        if id.starts_with("V_AV1") {
            return Self::Av1;
        }
        if id.starts_with("V_VP9") {
            return Self::Vp9;
        }
        if id.starts_with("V_VP8") {
            return Self::Vp8;
        }
        if id.starts_with("V_MPEG2") {
            return Self::Mpeg2;
        }
        // Every other V_MPEG4/ISO/* profile is MPEG-4 Part 2. Checked after
        // AVC, which shares the prefix and is not Part 2.
        if id.starts_with("V_MPEG4") || id.starts_with("V_MS/VFW/FOURCC") {
            return Self::Mpeg4;
        }
        if id.starts_with("V_MS/VFW/WVC1") || id.starts_with("V_VC1") {
            return Self::Vc1;
        }
        Self::Unknown
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

    /// Normalise an Apple `AudioFormatID`, as its four-character code.
    ///
    /// These are four-character codes too, and several of them are padded to
    /// four with a trailing space (`"aac "`) or a leading dot (`".mp3"`), so
    /// the padding is significant and must not be trimmed away. Only the
    /// surrounding whitespace Swift might add is removed.
    ///
    /// `alac` is deliberately absent: there is no `AudioCodec::Alac` variant,
    /// and inventing one here would claim a match that `from_probe` cannot
    /// make, which is the failure mode this whole module exists to prevent.
    #[must_use]
    pub fn from_apple_fourcc(code: &str) -> Self {
        match code.to_ascii_lowercase().as_str() {
            "aac " | "aach" | "aacp" => Self::Aac,
            "ac-3" | "sac3" => Self::Ac3,
            "ec-3" => Self::Eac3,
            "flac" => Self::Flac,
            "opus" => Self::Opus,
            ".mp3" => Self::Mp3,
            "lpcm" => Self::Pcm,
            _ => Self::Unknown,
        }
    }

    /// Normalise a Matroska `CodecID`, as the container spells it.
    #[must_use]
    pub fn from_matroska_codec_id(codec_id: &str) -> Self {
        let id = codec_id.trim().to_ascii_uppercase();
        if id.starts_with("A_AAC") {
            return Self::Aac;
        }
        if id.starts_with("A_OPUS") {
            return Self::Opus;
        }
        // Ordered before A_AC3: "A_EAC3" does not start with "A_AC3", but
        // some muxers write "A_AC3/BSID10" for E-AC-3, which does.
        if id.starts_with("A_EAC3") || id.starts_with("A_AC3/BSID10") {
            return Self::Eac3;
        }
        if id.starts_with("A_AC3") {
            return Self::Ac3;
        }
        if id.starts_with("A_DTS") {
            return Self::Dts;
        }
        if id.starts_with("A_TRUEHD") || id.starts_with("A_MLP") {
            return Self::TrueHd;
        }
        if id.starts_with("A_FLAC") {
            return Self::Flac;
        }
        if id.starts_with("A_VORBIS") {
            return Self::Vorbis;
        }
        if id.starts_with("A_MPEG/L3") {
            return Self::Mp3;
        }
        if id.starts_with("A_PCM") {
            return Self::Pcm;
        }
        Self::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vocabulary_meets_on_the_same_video_codec() {
        // The whole point of the four tables: FFmpeg's probe name, Android's
        // MIME, Apple's four-character code and Matroska's CodecID for one
        // codec must all normalise to one value, or nothing matches. A source
        // probed as "hevc" has to be recognisable as playable by a device that
        // reported "hvc1", after being demuxed from a track labelled
        // "V_MPEGH/ISO/HEVC".
        for (probe, mime, fourcc, codec_id, expected) in [
            (
                "h264",
                "video/avc",
                "avc1",
                "V_MPEG4/ISO/AVC",
                VideoCodec::H264,
            ),
            (
                "hevc",
                "video/hevc",
                "hvc1",
                "V_MPEGH/ISO/HEVC",
                VideoCodec::H265,
            ),
            ("av1", "video/av01", "av01", "V_AV1", VideoCodec::Av1),
            (
                "vp9",
                "video/x-vnd.on2.vp9",
                "vp09",
                "V_VP9",
                VideoCodec::Vp9,
            ),
        ] {
            assert_eq!(VideoCodec::from_probe(probe), expected);
            assert_eq!(VideoCodec::from_android_mime(mime), expected);
            assert_eq!(VideoCodec::from_apple_fourcc(fourcc), expected);
            assert_eq!(VideoCodec::from_matroska_codec_id(codec_id), expected);
        }
    }

    #[test]
    fn every_vocabulary_meets_on_the_same_audio_codec() {
        // Apple's AudioFormatIDs are padded to four characters; the padding is
        // part of the code, so "aac " must match and "aac" must not.
        for (probe, mime, fourcc, codec_id, expected) in [
            ("aac", "audio/mp4a-latm", "aac ", "A_AAC", AudioCodec::Aac),
            ("opus", "audio/opus", "opus", "A_OPUS", AudioCodec::Opus),
            ("ac3", "audio/ac3", "ac-3", "A_AC3", AudioCodec::Ac3),
            ("eac3", "audio/eac3", "ec-3", "A_EAC3", AudioCodec::Eac3),
            ("flac", "audio/flac", "flac", "A_FLAC", AudioCodec::Flac),
        ] {
            assert_eq!(AudioCodec::from_probe(probe), expected);
            assert_eq!(AudioCodec::from_android_mime(mime), expected);
            assert_eq!(AudioCodec::from_apple_fourcc(fourcc), expected);
            assert_eq!(AudioCodec::from_matroska_codec_id(codec_id), expected);
        }
    }

    #[test]
    fn codecs_with_no_apple_decoder_still_normalise_from_the_container() {
        // The Apple table has no entry for these, but the Matroska table must:
        // a track the sample-buffer engine cannot play still has to be
        // identified, so `select_source` can reject it with a reason rather
        // than reporting an unknown codec.
        for (codec_id, expected) in [
            ("A_DTS", AudioCodec::Dts),
            ("A_TRUEHD", AudioCodec::TrueHd),
            ("A_VORBIS", AudioCodec::Vorbis),
        ] {
            assert_eq!(AudioCodec::from_matroska_codec_id(codec_id), expected);
            assert_eq!(AudioCodec::from_apple_fourcc(codec_id), AudioCodec::Unknown);
        }
    }

    #[test]
    fn eac3_written_with_an_ac3_codec_id_is_not_mistaken_for_ac3() {
        // Some muxers spell E-AC-3 as A_AC3/BSID10, which is a prefix match for
        // A_AC3. Getting this backwards would have the client hand an E-AC-3
        // stream to an AC-3 decoder and produce noise, not an error.
        assert_eq!(
            AudioCodec::from_matroska_codec_id("A_AC3/BSID10"),
            AudioCodec::Eac3
        );
        assert_eq!(
            AudioCodec::from_matroska_codec_id("A_AC3/BSID9"),
            AudioCodec::Ac3
        );
    }

    #[test]
    fn avc_is_not_collapsed_into_the_mpeg4_part_2_prefix_it_shares() {
        // V_MPEG4/ISO/AVC and V_MPEG4/ISO/SP share a prefix and are different
        // codecs. Ordering the AVC check first is what makes this hold.
        assert_eq!(
            VideoCodec::from_matroska_codec_id("V_MPEG4/ISO/AVC"),
            VideoCodec::H264
        );
        assert_eq!(
            VideoCodec::from_matroska_codec_id("V_MPEG4/ISO/SP"),
            VideoCodec::Mpeg4
        );
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
        assert_eq!(VideoCodec::from_apple_fourcc(" AVC1 "), VideoCodec::H264);
        assert_eq!(
            VideoCodec::from_matroska_codec_id(" v_mpeg4/iso/avc "),
            VideoCodec::H264
        );
    }

    #[test]
    fn an_apple_four_character_code_keeps_its_padding() {
        // "aac " is the AudioFormatID; "aac" is not one. Trimming the trailing
        // space would silently accept a value Core Audio never produces, and
        // hide a bug in whatever built it.
        assert_eq!(AudioCodec::from_apple_fourcc("aac "), AudioCodec::Aac);
        assert_eq!(AudioCodec::from_apple_fourcc("aac"), AudioCodec::Unknown);
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
