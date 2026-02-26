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

#[derive(Eq, PartialEq, Copy, Clone, Hash)]
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
    // Other
    Other(ffmpeg::ffi::AVCodecID),
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
            ffmpeg::codec::Id::H265 => CodecId::H265,
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
            id => CodecId::Other(id.into()),
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
