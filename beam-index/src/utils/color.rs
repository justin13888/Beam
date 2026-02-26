use ffmpeg_next as ffmpeg;

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum PixelFormat {
    YUV420P10LE,
    YUV420P10BE,
    YUV420P12LE,
    YUV420P12BE,
    YUV420P16LE,
    YUV420P16BE,
    Other(ffmpeg::ffi::AVPixelFormat),
    None,
}

impl PixelFormat {
    /// Get bit depth from pixel format
    /// Returns None if unknown.
    pub fn bit_depth(&self) -> Option<u8> {
        match self {
            PixelFormat::YUV420P10LE | PixelFormat::YUV420P10BE => Some(10),
            PixelFormat::YUV420P12LE | PixelFormat::YUV420P12BE => Some(12),
            PixelFormat::YUV420P16LE | PixelFormat::YUV420P16BE => Some(16),
            _ => None,
        } // TODO: Include everything supported by FFmpeg 8.0+
    }
}

impl From<ffmpeg::format::Pixel> for PixelFormat {
    fn from(pixel: ffmpeg::format::Pixel) -> Self {
        match pixel {
            ffmpeg::format::Pixel::YUV420P10LE => PixelFormat::YUV420P10LE,
            ffmpeg::format::Pixel::YUV420P10BE => PixelFormat::YUV420P10BE,
            ffmpeg::format::Pixel::YUV420P12LE => PixelFormat::YUV420P12LE,
            ffmpeg::format::Pixel::YUV420P12BE => PixelFormat::YUV420P12BE,
            ffmpeg::format::Pixel::YUV420P16LE => PixelFormat::YUV420P16LE,
            ffmpeg::format::Pixel::YUV420P16BE => PixelFormat::YUV420P16BE,
            ffmpeg::format::Pixel::None => PixelFormat::None,
            other => PixelFormat::Other(other.into()),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorSpace {
    RGB,
    BT709,
    BT470BG,
    SMPTE170M,
    SMPTE240M,
    BT2020NCL,
    BT2020CL,
    Other(ffmpeg::ffi::AVColorSpace),
}

impl ColorSpace {
    /// Get a human-readable description of the color space
    pub fn description(&self) -> &'static str {
        match self {
            ColorSpace::RGB => "RGB",
            ColorSpace::BT709 => "BT.709",
            ColorSpace::BT470BG => "BT.470 BG",
            ColorSpace::SMPTE170M => "SMPTE-170M",
            ColorSpace::SMPTE240M => "SMPTE-240M",
            ColorSpace::BT2020NCL => "BT.2020 Non-Constant Luminance",
            ColorSpace::BT2020CL => "BT.2020 Constant Luminance",
            _ => "Unknown",
        } // TODO: Include everything supported by FFmpeg 8.0+
    }
}

impl From<ffmpeg::color::Space> for ColorSpace {
    fn from(space: ffmpeg::color::Space) -> Self {
        match space {
            ffmpeg::color::Space::RGB => ColorSpace::RGB,
            ffmpeg::color::Space::BT709 => ColorSpace::BT709,
            ffmpeg::color::Space::BT470BG => ColorSpace::BT470BG,
            ffmpeg::color::Space::SMPTE170M => ColorSpace::SMPTE170M,
            ffmpeg::color::Space::SMPTE240M => ColorSpace::SMPTE240M,
            ffmpeg::color::Space::BT2020NCL => ColorSpace::BT2020NCL,
            ffmpeg::color::Space::BT2020CL => ColorSpace::BT2020CL,
            other => ColorSpace::Other(other.into()),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorRange {
    MPEG,
    JPEG,
    Other(ffmpeg::ffi::AVColorRange),
    Unspecified,
}

impl ColorRange {
    /// Get a human-readable description of the color range
    pub fn description(&self) -> &'static str {
        match self {
            ColorRange::MPEG => "Limited (TV)",
            ColorRange::JPEG => "Full (PC)",
            _ => "Unknown",
        } // TODO: Include everything supported by FFmpeg 8.0+
    }
}

impl From<ffmpeg::color::Range> for ColorRange {
    fn from(range: ffmpeg::color::Range) -> Self {
        match range {
            ffmpeg::color::Range::MPEG => ColorRange::MPEG,
            ffmpeg::color::Range::JPEG => ColorRange::JPEG,
            ffmpeg::color::Range::Unspecified => ColorRange::Unspecified,
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorPrimaries {
    BT709,
    BT470BG,
    SMPTE170M,
    SMPTE240M,
    BT2020,
    SMPTE428,
    SMPTE431,
    SMPTE432,
    Other(ffmpeg::ffi::AVColorPrimaries),
}

impl ColorPrimaries {
    /// Get a human-readable description of the color primaries
    pub fn description(&self) -> &'static str {
        match self {
            ColorPrimaries::BT709 => "BT.709",
            ColorPrimaries::BT470BG => "BT.470 BG",
            ColorPrimaries::SMPTE170M => "SMPTE-170M",
            ColorPrimaries::SMPTE240M => "SMPTE-240M",
            ColorPrimaries::BT2020 => "BT.2020",
            ColorPrimaries::SMPTE428 => "SMPTE-428",
            ColorPrimaries::SMPTE431 => "SMPTE-431",
            ColorPrimaries::SMPTE432 => "SMPTE-432",
            _ => "Unknown",
        } // TODO: Include everything supported by FFmpeg 8.0+
    }
}

impl From<ffmpeg::color::Primaries> for ColorPrimaries {
    fn from(primaries: ffmpeg::color::Primaries) -> Self {
        match primaries {
            ffmpeg::color::Primaries::BT709 => ColorPrimaries::BT709,
            ffmpeg::color::Primaries::BT470BG => ColorPrimaries::BT470BG,
            ffmpeg::color::Primaries::SMPTE170M => ColorPrimaries::SMPTE170M,
            ffmpeg::color::Primaries::SMPTE240M => ColorPrimaries::SMPTE240M,
            ffmpeg::color::Primaries::BT2020 => ColorPrimaries::BT2020,
            ffmpeg::color::Primaries::SMPTE428 => ColorPrimaries::SMPTE428,
            ffmpeg::color::Primaries::SMPTE431 => ColorPrimaries::SMPTE431,
            ffmpeg::color::Primaries::SMPTE432 => ColorPrimaries::SMPTE432,
            other => ColorPrimaries::Other(other.into()),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorTransferCharacteristic {
    BT709,
    SMPTE170M,
    SMPTE240M,
    SMPTE2084,
    AribStdB67,
    Other(ffmpeg::ffi::AVColorTransferCharacteristic),
}

impl ColorTransferCharacteristic {
    /// Get a human-readable description of the transfer characteristic
    pub fn description(&self) -> &'static str {
        match self {
            ColorTransferCharacteristic::BT709 => "BT.709",
            ColorTransferCharacteristic::SMPTE170M => "SMPTE-170M",
            ColorTransferCharacteristic::SMPTE240M => "SMPTE-240M",
            ColorTransferCharacteristic::SMPTE2084 => "SMPTE-2084 (PQ)",
            ColorTransferCharacteristic::AribStdB67 => "HLG (Hybrid Log-Gamma)",
            _ => "Unknown",
        } // TODO: Include everything supported by FFmpeg 8.0+
    }

    /// Check if this is an HDR transfer characteristic
    pub fn is_hdr(&self) -> bool {
        matches!(
            self,
            ColorTransferCharacteristic::SMPTE2084 | ColorTransferCharacteristic::AribStdB67
        )
    }
}

impl From<ffmpeg::color::TransferCharacteristic> for ColorTransferCharacteristic {
    fn from(transfer: ffmpeg::color::TransferCharacteristic) -> Self {
        match transfer {
            ffmpeg::color::TransferCharacteristic::BT709 => ColorTransferCharacteristic::BT709,
            ffmpeg::color::TransferCharacteristic::SMPTE170M => {
                ColorTransferCharacteristic::SMPTE170M
            }
            ffmpeg::color::TransferCharacteristic::SMPTE240M => {
                ColorTransferCharacteristic::SMPTE240M
            }
            ffmpeg::color::TransferCharacteristic::SMPTE2084 => {
                ColorTransferCharacteristic::SMPTE2084
            }
            ffmpeg::color::TransferCharacteristic::ARIB_STD_B67 => {
                ColorTransferCharacteristic::AribStdB67
            }
            other => ColorTransferCharacteristic::Other(other.into()),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ChromaLocation {
    Left,
    Center,
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
    Other(ffmpeg::ffi::AVChromaLocation),
    Unspecified,
}

impl ChromaLocation {
    /// Get a human-readable description of the chroma location
    pub fn description(&self) -> &'static str {
        match self {
            ChromaLocation::Left => "Left",
            ChromaLocation::Center => "Center",
            ChromaLocation::TopLeft => "Top Left",
            ChromaLocation::Top => "Top",
            ChromaLocation::BottomLeft => "Bottom Left",
            ChromaLocation::Bottom => "Bottom",
            _ => "Unknown",
        } // TODO: Include everything supported by FFmpeg 8.0+
    }
}

impl From<ffmpeg::chroma::Location> for ChromaLocation {
    fn from(location: ffmpeg::chroma::Location) -> Self {
        match location {
            ffmpeg::chroma::Location::Left => ChromaLocation::Left,
            ffmpeg::chroma::Location::Center => ChromaLocation::Center,
            ffmpeg::chroma::Location::TopLeft => ChromaLocation::TopLeft,
            ffmpeg::chroma::Location::Top => ChromaLocation::Top,
            ffmpeg::chroma::Location::BottomLeft => ChromaLocation::BottomLeft,
            ffmpeg::chroma::Location::Bottom => ChromaLocation::Bottom,
            ffmpeg::chroma::Location::Unspecified => ChromaLocation::Unspecified,
        }
    }
}
