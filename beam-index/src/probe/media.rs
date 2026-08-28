use ffmpeg_next as ffmpeg;

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum MediaType {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

impl MediaType {
    /// Check if this is a video stream
    pub fn is_video(&self) -> bool {
        matches!(self, MediaType::Video)
    }

    /// Check if this is an audio stream
    pub fn is_audio(&self) -> bool {
        matches!(self, MediaType::Audio)
    }

    /// Check if this is a subtitle stream
    pub fn is_subtitle(&self) -> bool {
        matches!(self, MediaType::Subtitle)
    }

    /// Check if this is a data stream
    pub fn is_data(&self) -> bool {
        matches!(self, MediaType::Data)
    }

    /// Check if this is an attachment stream
    pub fn is_attachment(&self) -> bool {
        matches!(self, MediaType::Attachment)
    }

    /// Get a human-readable description of the media type
    pub fn description(&self) -> &'static str {
        match self {
            MediaType::Video => "Video",
            MediaType::Audio => "Audio",
            MediaType::Subtitle => "Subtitle",
            MediaType::Data => "Data",
            MediaType::Attachment => "Attachment",
            MediaType::Unknown => "Unknown",
        }
    }
}

impl From<ffmpeg::media::Type> for MediaType {
    fn from(media_type: ffmpeg::media::Type) -> Self {
        match media_type {
            ffmpeg::media::Type::Video => MediaType::Video,
            ffmpeg::media::Type::Audio => MediaType::Audio,
            ffmpeg::media::Type::Subtitle => MediaType::Subtitle,
            ffmpeg::media::Type::Data => MediaType::Data,
            ffmpeg::media::Type::Attachment => MediaType::Attachment,
            ffmpeg::media::Type::Unknown => MediaType::Unknown,
        }
    }
}

#[derive(Eq, PartialEq, Clone, Hash)]
pub enum CodecId {
    // Video codecs
    H264,
    H265,
    VP8,
    VP9,
    AV1,
    MPEG1VIDEO,
    MPEG2VIDEO,
    MPEG4,
    // Audio codecs
    AAC,
    MP3,
    AC3,
    EAC3,
    DTS,
    TRUEHD,
    FLAC,
    VORBIS,
    OPUS,
    // Subtitle codecs
    SUBRIP,
    ASS,
    WEBVTT,
    // Other: codec name string (e.g. from `ffmpeg::codec::Id::name()`), not a
    // raw FFmpeg FFI type, so this enum never leaks FFI types out of the
    // probing layer.
    Other(String),
    None,
}

impl CodecId {
    /// Get the media type for this codec
    pub fn media_type(&self) -> MediaType {
        match self {
            // Video codecs
            CodecId::H264
            | CodecId::H265
            | CodecId::VP8
            | CodecId::VP9
            | CodecId::AV1
            | CodecId::MPEG1VIDEO
            | CodecId::MPEG2VIDEO
            | CodecId::MPEG4 => MediaType::Video,
            // Audio codecs
            CodecId::AAC
            | CodecId::MP3
            | CodecId::AC3
            | CodecId::EAC3
            | CodecId::DTS
            | CodecId::TRUEHD
            | CodecId::FLAC
            | CodecId::VORBIS
            | CodecId::OPUS => MediaType::Audio,
            // Subtitle codecs
            CodecId::SUBRIP | CodecId::ASS | CodecId::WEBVTT => MediaType::Subtitle,
            _ => MediaType::Unknown,
        }
    }

    /// Check if this is a video codec
    pub fn is_video(&self) -> bool {
        self.media_type().is_video()
    }

    /// Check if this is an audio codec
    pub fn is_audio(&self) -> bool {
        self.media_type().is_audio()
    }

    /// Check if this is a subtitle codec
    pub fn is_subtitle(&self) -> bool {
        self.media_type().is_subtitle()
    }

    /// Get a human-readable name for the codec
    pub fn name(&self) -> &'static str {
        match self {
            // Video codecs
            CodecId::H264 => "H.264/AVC",
            CodecId::H265 => "H.265/HEVC",
            CodecId::VP8 => "VP8",
            CodecId::VP9 => "VP9",
            CodecId::AV1 => "AV1",
            CodecId::MPEG1VIDEO => "MPEG-1",
            CodecId::MPEG2VIDEO => "MPEG-2",
            CodecId::MPEG4 => "MPEG-4",
            // Audio codecs
            CodecId::AAC => "AAC",
            CodecId::MP3 => "MP3",
            CodecId::AC3 => "AC-3",
            CodecId::EAC3 => "E-AC-3",
            CodecId::DTS => "DTS",
            CodecId::TRUEHD => "TrueHD",
            CodecId::FLAC => "FLAC",
            CodecId::VORBIS => "Vorbis",
            CodecId::OPUS => "Opus",
            // Subtitle codecs
            CodecId::SUBRIP => "SubRip",
            CodecId::ASS => "ASS/SSA",
            CodecId::WEBVTT => "WebVTT",
            _ => "Unknown",
        }
    }

    /// Check if this codec supports hardware acceleration
    pub fn supports_hardware_acceleration(&self) -> bool {
        matches!(
            self,
            CodecId::H264 | CodecId::H265 | CodecId::VP9 | CodecId::AV1
        )
    }

    /// Check if this is a lossless codec
    pub fn is_lossless(&self) -> bool {
        matches!(
            self,
            CodecId::FLAC | CodecId::SUBRIP | CodecId::ASS | CodecId::WEBVTT
        )
    }
}

impl From<ffmpeg::codec::Id> for CodecId {
    fn from(codec_id: ffmpeg::codec::Id) -> Self {
        match codec_id {
            ffmpeg::codec::Id::H264 => CodecId::H264,
            // ffmpeg-next maps AV_CODEC_ID_HEVC to `Id::HEVC`, never to its
            // `Id::H265` alias; match both so real HEVC streams don't fall
            // through to `Other("hevc")`.
            ffmpeg::codec::Id::HEVC | ffmpeg::codec::Id::H265 => CodecId::H265,
            ffmpeg::codec::Id::VP8 => CodecId::VP8,
            ffmpeg::codec::Id::VP9 => CodecId::VP9,
            ffmpeg::codec::Id::AV1 => CodecId::AV1,
            ffmpeg::codec::Id::MPEG1VIDEO => CodecId::MPEG1VIDEO,
            ffmpeg::codec::Id::MPEG2VIDEO => CodecId::MPEG2VIDEO,
            ffmpeg::codec::Id::MPEG4 => CodecId::MPEG4,
            ffmpeg::codec::Id::AAC => CodecId::AAC,
            ffmpeg::codec::Id::MP3 => CodecId::MP3,
            ffmpeg::codec::Id::AC3 => CodecId::AC3,
            ffmpeg::codec::Id::EAC3 => CodecId::EAC3,
            ffmpeg::codec::Id::DTS => CodecId::DTS,
            ffmpeg::codec::Id::TRUEHD => CodecId::TRUEHD,
            ffmpeg::codec::Id::FLAC => CodecId::FLAC,
            ffmpeg::codec::Id::VORBIS => CodecId::VORBIS,
            ffmpeg::codec::Id::OPUS => CodecId::OPUS,
            ffmpeg::codec::Id::SUBRIP => CodecId::SUBRIP,
            ffmpeg::codec::Id::ASS => CodecId::ASS,
            ffmpeg::codec::Id::WEBVTT => CodecId::WEBVTT,
            ffmpeg::codec::Id::None => CodecId::None,
            id => CodecId::Other(id.name().to_string()),
        }
    }
}

impl std::fmt::Display for CodecId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl std::fmt::Debug for CodecId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecId::Other(id) => write!(f, "Other({:?})", id),
            _ => write!(f, "{}", self.name()),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum Discard {
    None,
    Default,
    NonReference,
    Bidirectional,
    NonIntra,
    NonKey,
    All,
}

impl Discard {
    /// Check if this stream should be discarded
    pub fn should_discard(&self) -> bool {
        !matches!(self, Discard::Default)
    }

    /// Get a human-readable description of the discard setting
    pub fn description(&self) -> &'static str {
        match self {
            Discard::None => "None",
            Discard::Default => "Default",
            Discard::NonReference => "Non-Reference",
            Discard::Bidirectional => "Bidirectional",
            Discard::NonIntra => "Non-Intra",
            Discard::NonKey => "Non-Key",
            Discard::All => "All",
        }
    }
}

impl From<ffmpeg::Discard> for Discard {
    fn from(discard: ffmpeg::Discard) -> Self {
        match discard {
            ffmpeg::Discard::None => Discard::None,
            ffmpeg::Discard::Default => Discard::Default,
            ffmpeg::Discard::NonReference => Discard::NonReference,
            ffmpeg::Discard::Bidirectional => Discard::Bidirectional,
            ffmpeg::Discard::NonIntra => Discard::NonIntra,
            ffmpeg::Discard::NonKey => Discard::NonKey,
            ffmpeg::Discard::All => Discard::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every codec this module names explicitly. The `Other`/`None` fallbacks
    /// are covered separately.
    const NAMED: &[(ffmpeg::codec::Id, CodecId)] = &[
        (ffmpeg::codec::Id::H264, CodecId::H264),
        (ffmpeg::codec::Id::HEVC, CodecId::H265),
        (ffmpeg::codec::Id::VP8, CodecId::VP8),
        (ffmpeg::codec::Id::VP9, CodecId::VP9),
        (ffmpeg::codec::Id::AV1, CodecId::AV1),
        (ffmpeg::codec::Id::MPEG1VIDEO, CodecId::MPEG1VIDEO),
        (ffmpeg::codec::Id::MPEG2VIDEO, CodecId::MPEG2VIDEO),
        (ffmpeg::codec::Id::MPEG4, CodecId::MPEG4),
        (ffmpeg::codec::Id::AAC, CodecId::AAC),
        (ffmpeg::codec::Id::MP3, CodecId::MP3),
        (ffmpeg::codec::Id::AC3, CodecId::AC3),
        (ffmpeg::codec::Id::EAC3, CodecId::EAC3),
        (ffmpeg::codec::Id::DTS, CodecId::DTS),
        (ffmpeg::codec::Id::TRUEHD, CodecId::TRUEHD),
        (ffmpeg::codec::Id::FLAC, CodecId::FLAC),
        (ffmpeg::codec::Id::VORBIS, CodecId::VORBIS),
        (ffmpeg::codec::Id::OPUS, CodecId::OPUS),
        (ffmpeg::codec::Id::SUBRIP, CodecId::SUBRIP),
        (ffmpeg::codec::Id::ASS, CodecId::ASS),
        (ffmpeg::codec::Id::WEBVTT, CodecId::WEBVTT),
    ];

    #[test]
    fn every_named_codec_is_classified_the_way_ffmpeg_classifies_it() {
        // Derived, not mirrored: the expectation comes from FFmpeg's own
        // `Id::medium()` rather than from a second copy of the match arms in
        // this module, so a codec put in the wrong arm here fails even though
        // both tables would have been edited together.
        for (ffmpeg_id, expected) in NAMED {
            let converted = CodecId::from(*ffmpeg_id);
            assert_eq!(&converted, expected, "conversion of {ffmpeg_id:?}");
            assert_eq!(
                converted.media_type(),
                MediaType::from(ffmpeg_id.medium()),
                "{ffmpeg_id:?} is classified differently here than by FFmpeg"
            );
        }
    }

    #[test]
    fn hevc_and_its_h265_alias_are_the_same_codec() {
        // ffmpeg-next maps AV_CODEC_ID_HEVC to `Id::HEVC`; missing the alias
        // sent every real HEVC stream to `Other("hevc")`.
        assert_eq!(CodecId::from(ffmpeg::codec::Id::HEVC), CodecId::H265);
        assert_eq!(CodecId::from(ffmpeg::codec::Id::H265), CodecId::H265);
    }

    #[test]
    fn an_unrecognised_codec_keeps_its_ffmpeg_name_rather_than_being_dropped() {
        // The catch-all must preserve enough to diagnose a file, and must not
        // leak an FFI type out of the probing layer.
        let converted = CodecId::from(ffmpeg::codec::Id::THEORA);
        assert_eq!(converted, CodecId::Other("theora".to_string()));
        assert_eq!(converted.media_type(), MediaType::Unknown);
        assert_eq!(format!("{converted:?}"), "Other(\"theora\")");
    }

    #[test]
    fn the_absence_of_a_codec_is_not_an_unknown_codec() {
        assert_eq!(CodecId::from(ffmpeg::codec::Id::None), CodecId::None);
        assert_eq!(CodecId::None.media_type(), MediaType::Unknown);
    }

    #[test]
    fn the_is_predicates_agree_with_the_media_type() {
        for (_, codec) in NAMED {
            assert_eq!(codec.is_video(), codec.media_type().is_video());
            assert_eq!(codec.is_audio(), codec.media_type().is_audio());
            assert_eq!(codec.is_subtitle(), codec.media_type().is_subtitle());
        }
    }

    #[test]
    fn every_named_codec_has_a_distinct_display_name() {
        let mut seen = std::collections::HashSet::new();
        for (_, codec) in NAMED {
            assert!(
                seen.insert(codec.name()),
                "{codec:?} shares a display name with another codec"
            );
            assert_ne!(codec.name(), "Unknown", "{codec:?} has no display name");
            assert_eq!(format!("{codec}"), codec.name());
        }
    }

    #[test]
    fn hardware_acceleration_is_claimed_only_for_video() {
        // Claiming it for an audio or subtitle codec would be nonsense; the
        // property is checked against the classification rather than restated.
        for (_, codec) in NAMED {
            if codec.supports_hardware_acceleration() {
                assert!(codec.is_video(), "{codec:?} is not a video codec");
            }
        }
        assert!(CodecId::H264.supports_hardware_acceleration());
        assert!(CodecId::AV1.supports_hardware_acceleration());
        assert!(
            !CodecId::VP8.supports_hardware_acceleration(),
            "VP8 is deliberately excluded"
        );
    }

    #[test]
    fn lossless_covers_flac_and_every_text_subtitle_format() {
        // Text subtitles are trivially lossless; among audio codecs only FLAC
        // is. Anything else claiming losslessness is a mistake.
        for (_, codec) in NAMED {
            if codec.is_subtitle() {
                assert!(codec.is_lossless(), "{codec:?} is text and so lossless");
            }
            if codec.is_video() {
                assert!(!codec.is_lossless(), "{codec:?} is a lossy video codec");
            }
        }
        assert!(CodecId::FLAC.is_lossless());
        assert!(!CodecId::MP3.is_lossless());
        assert!(!CodecId::OPUS.is_lossless());
    }

    #[test]
    fn every_media_type_describes_itself_distinctly() {
        // The description labels a stream in the admin file view; one shared
        // string makes every track in a file look identical.
        let mut seen = std::collections::HashSet::new();
        for media_type in [
            MediaType::Video,
            MediaType::Audio,
            MediaType::Subtitle,
            MediaType::Data,
            MediaType::Attachment,
            MediaType::Unknown,
        ] {
            let description = media_type.description();
            assert!(!description.is_empty(), "{media_type:?} has no description");
            assert!(
                seen.insert(description),
                "{media_type:?} shares a description with another type"
            );
        }
        assert_eq!(MediaType::Video.description(), "Video");
        assert_eq!(MediaType::Subtitle.description(), "Subtitle");
    }

    #[test]
    fn media_types_round_trip_from_ffmpeg_and_report_themselves() {
        for (ffmpeg_type, expected) in [
            (ffmpeg::media::Type::Video, MediaType::Video),
            (ffmpeg::media::Type::Audio, MediaType::Audio),
            (ffmpeg::media::Type::Subtitle, MediaType::Subtitle),
            (ffmpeg::media::Type::Data, MediaType::Data),
            (ffmpeg::media::Type::Attachment, MediaType::Attachment),
            (ffmpeg::media::Type::Unknown, MediaType::Unknown),
        ] {
            let converted = MediaType::from(ffmpeg_type);
            assert_eq!(converted, expected);
            // Exactly one predicate answers true for each type (Unknown: none).
            let set = [
                converted.is_video(),
                converted.is_audio(),
                converted.is_subtitle(),
                converted.is_data(),
                converted.is_attachment(),
            ];
            let true_count = set.iter().filter(|b| **b).count();
            let expected_count = usize::from(converted != MediaType::Unknown);
            assert_eq!(true_count, expected_count, "for {converted:?}");
        }
    }

    #[test]
    fn every_discard_setting_describes_itself_distinctly() {
        let mut seen = std::collections::HashSet::new();
        for discard in [
            Discard::None,
            Discard::Default,
            Discard::NonReference,
            Discard::Bidirectional,
            Discard::NonIntra,
            Discard::NonKey,
            Discard::All,
        ] {
            let description = discard.description();
            assert!(!description.is_empty(), "{discard:?} has no description");
            assert!(
                seen.insert(description),
                "{discard:?} shares a description with another setting"
            );
        }
        assert_eq!(Discard::NonReference.description(), "Non-Reference");
    }

    #[test]
    fn only_the_default_discard_setting_keeps_a_stream() {
        // `should_discard` inverts a single variant; getting it backwards
        // would silently drop every stream in the file.
        for (ffmpeg_discard, expected) in [
            (ffmpeg::Discard::None, Discard::None),
            (ffmpeg::Discard::Default, Discard::Default),
            (ffmpeg::Discard::NonReference, Discard::NonReference),
            (ffmpeg::Discard::Bidirectional, Discard::Bidirectional),
            (ffmpeg::Discard::NonIntra, Discard::NonIntra),
            (ffmpeg::Discard::NonKey, Discard::NonKey),
            (ffmpeg::Discard::All, Discard::All),
        ] {
            let converted = Discard::from(ffmpeg_discard);
            assert_eq!(converted, expected);
            assert_eq!(
                converted.should_discard(),
                converted != Discard::Default,
                "for {converted:?}"
            );
        }
    }
}
