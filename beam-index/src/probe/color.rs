use ffmpeg_next as ffmpeg;

// ── PixelFormat ─────────────────────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum PixelFormat {
    YUV420P,
    YUV422P,
    YUV444P,
    YUV410P,
    YUV411P,
    YUV440P,
    YUVJ420P,
    YUVJ422P,
    YUVJ444P,
    YUVJ440P,
    NV12,
    NV21,
    GRAY8,
    RGB24,
    BGR24,
    ARGB,
    RGBA,
    ABGR,
    BGRA,
    // 9-bit
    YUV420P9LE,
    YUV420P9BE,
    YUV422P9LE,
    YUV422P9BE,
    YUV444P9LE,
    YUV444P9BE,
    // 10-bit
    YUV420P10LE,
    YUV420P10BE,
    YUV422P10LE,
    YUV422P10BE,
    YUV444P10LE,
    YUV444P10BE,
    // 12-bit
    YUV420P12LE,
    YUV420P12BE,
    YUV422P12LE,
    YUV422P12BE,
    YUV444P12LE,
    YUV444P12BE,
    // 16-bit
    YUV420P16LE,
    YUV420P16BE,
    YUV422P16LE,
    YUV422P16BE,
    YUV444P16LE,
    YUV444P16BE,
    // Alpha variants
    YUVA420P,
    // Hardware-accelerated surface formats
    VAAPI,
    #[allow(non_camel_case_types)]
    DXVA2_VLD,
    VDPAU,
    VIDEOTOOLBOX,
    D3D11,
    CUDA,
    QSV,
    VULKAN,
    // Planar GBR
    GBRP,
    GBRP10LE,
    GBRP12LE,
    // Catch-all for less common formats
    Other(ffmpeg::ffi::AVPixelFormat),
    None,
}

impl PixelFormat {
    /// Get bit depth from pixel format.
    /// Returns None if unknown.
    pub fn bit_depth(&self) -> Option<u8> {
        match self {
            PixelFormat::YUV420P
            | PixelFormat::YUV422P
            | PixelFormat::YUV444P
            | PixelFormat::YUV410P
            | PixelFormat::YUV411P
            | PixelFormat::YUV440P
            | PixelFormat::YUVJ420P
            | PixelFormat::YUVJ422P
            | PixelFormat::YUVJ444P
            | PixelFormat::YUVJ440P
            | PixelFormat::NV12
            | PixelFormat::NV21
            | PixelFormat::GRAY8
            | PixelFormat::RGB24
            | PixelFormat::BGR24
            | PixelFormat::ARGB
            | PixelFormat::RGBA
            | PixelFormat::ABGR
            | PixelFormat::BGRA
            | PixelFormat::YUVA420P
            | PixelFormat::GBRP => Some(8),

            PixelFormat::YUV420P9LE
            | PixelFormat::YUV420P9BE
            | PixelFormat::YUV422P9LE
            | PixelFormat::YUV422P9BE
            | PixelFormat::YUV444P9LE
            | PixelFormat::YUV444P9BE => Some(9),

            PixelFormat::YUV420P10LE
            | PixelFormat::YUV420P10BE
            | PixelFormat::YUV422P10LE
            | PixelFormat::YUV422P10BE
            | PixelFormat::YUV444P10LE
            | PixelFormat::YUV444P10BE
            | PixelFormat::GBRP10LE => Some(10),

            PixelFormat::YUV420P12LE
            | PixelFormat::YUV420P12BE
            | PixelFormat::YUV422P12LE
            | PixelFormat::YUV422P12BE
            | PixelFormat::YUV444P12LE
            | PixelFormat::YUV444P12BE
            | PixelFormat::GBRP12LE => Some(12),

            PixelFormat::YUV420P16LE
            | PixelFormat::YUV420P16BE
            | PixelFormat::YUV422P16LE
            | PixelFormat::YUV422P16BE
            | PixelFormat::YUV444P16LE
            | PixelFormat::YUV444P16BE => Some(16),

            // Hardware formats don't have a fixed bit depth
            PixelFormat::VAAPI
            | PixelFormat::DXVA2_VLD
            | PixelFormat::VDPAU
            | PixelFormat::VIDEOTOOLBOX
            | PixelFormat::D3D11
            | PixelFormat::CUDA
            | PixelFormat::QSV
            | PixelFormat::VULKAN => None,

            PixelFormat::Other(_) | PixelFormat::None => None,
        }
    }

    /// Check if this is a hardware-accelerated pixel format
    pub fn is_hardware(&self) -> bool {
        matches!(
            self,
            PixelFormat::VAAPI
                | PixelFormat::DXVA2_VLD
                | PixelFormat::VDPAU
                | PixelFormat::VIDEOTOOLBOX
                | PixelFormat::D3D11
                | PixelFormat::CUDA
                | PixelFormat::QSV
                | PixelFormat::VULKAN
        )
    }
}

impl From<ffmpeg::format::Pixel> for PixelFormat {
    fn from(pixel: ffmpeg::format::Pixel) -> Self {
        match pixel {
            ffmpeg::format::Pixel::YUV420P => PixelFormat::YUV420P,
            ffmpeg::format::Pixel::YUV422P => PixelFormat::YUV422P,
            ffmpeg::format::Pixel::YUV444P => PixelFormat::YUV444P,
            ffmpeg::format::Pixel::YUV410P => PixelFormat::YUV410P,
            ffmpeg::format::Pixel::YUV411P => PixelFormat::YUV411P,
            ffmpeg::format::Pixel::YUV440P => PixelFormat::YUV440P,
            ffmpeg::format::Pixel::YUVJ420P => PixelFormat::YUVJ420P,
            ffmpeg::format::Pixel::YUVJ422P => PixelFormat::YUVJ422P,
            ffmpeg::format::Pixel::YUVJ444P => PixelFormat::YUVJ444P,
            ffmpeg::format::Pixel::YUVJ440P => PixelFormat::YUVJ440P,
            ffmpeg::format::Pixel::NV12 => PixelFormat::NV12,
            ffmpeg::format::Pixel::NV21 => PixelFormat::NV21,
            ffmpeg::format::Pixel::GRAY8 => PixelFormat::GRAY8,
            ffmpeg::format::Pixel::RGB24 => PixelFormat::RGB24,
            ffmpeg::format::Pixel::BGR24 => PixelFormat::BGR24,
            ffmpeg::format::Pixel::ARGB => PixelFormat::ARGB,
            ffmpeg::format::Pixel::RGBA => PixelFormat::RGBA,
            ffmpeg::format::Pixel::ABGR => PixelFormat::ABGR,
            ffmpeg::format::Pixel::BGRA => PixelFormat::BGRA,
            // 9-bit
            ffmpeg::format::Pixel::YUV420P9LE => PixelFormat::YUV420P9LE,
            ffmpeg::format::Pixel::YUV420P9BE => PixelFormat::YUV420P9BE,
            ffmpeg::format::Pixel::YUV422P9LE => PixelFormat::YUV422P9LE,
            ffmpeg::format::Pixel::YUV422P9BE => PixelFormat::YUV422P9BE,
            ffmpeg::format::Pixel::YUV444P9LE => PixelFormat::YUV444P9LE,
            ffmpeg::format::Pixel::YUV444P9BE => PixelFormat::YUV444P9BE,
            // 10-bit
            ffmpeg::format::Pixel::YUV420P10LE => PixelFormat::YUV420P10LE,
            ffmpeg::format::Pixel::YUV420P10BE => PixelFormat::YUV420P10BE,
            ffmpeg::format::Pixel::YUV422P10LE => PixelFormat::YUV422P10LE,
            ffmpeg::format::Pixel::YUV422P10BE => PixelFormat::YUV422P10BE,
            ffmpeg::format::Pixel::YUV444P10LE => PixelFormat::YUV444P10LE,
            ffmpeg::format::Pixel::YUV444P10BE => PixelFormat::YUV444P10BE,
            // 12-bit
            ffmpeg::format::Pixel::YUV420P12LE => PixelFormat::YUV420P12LE,
            ffmpeg::format::Pixel::YUV420P12BE => PixelFormat::YUV420P12BE,
            ffmpeg::format::Pixel::YUV422P12LE => PixelFormat::YUV422P12LE,
            ffmpeg::format::Pixel::YUV422P12BE => PixelFormat::YUV422P12BE,
            ffmpeg::format::Pixel::YUV444P12LE => PixelFormat::YUV444P12LE,
            ffmpeg::format::Pixel::YUV444P12BE => PixelFormat::YUV444P12BE,
            // 16-bit
            ffmpeg::format::Pixel::YUV420P16LE => PixelFormat::YUV420P16LE,
            ffmpeg::format::Pixel::YUV420P16BE => PixelFormat::YUV420P16BE,
            ffmpeg::format::Pixel::YUV422P16LE => PixelFormat::YUV422P16LE,
            ffmpeg::format::Pixel::YUV422P16BE => PixelFormat::YUV422P16BE,
            ffmpeg::format::Pixel::YUV444P16LE => PixelFormat::YUV444P16LE,
            ffmpeg::format::Pixel::YUV444P16BE => PixelFormat::YUV444P16BE,
            // Alpha
            ffmpeg::format::Pixel::YUVA420P => PixelFormat::YUVA420P,
            // Hardware
            ffmpeg::format::Pixel::DXVA2_VLD => PixelFormat::DXVA2_VLD,
            ffmpeg::format::Pixel::VDPAU => PixelFormat::VDPAU,
            ffmpeg::format::Pixel::VIDEOTOOLBOX => PixelFormat::VIDEOTOOLBOX,
            ffmpeg::format::Pixel::D3D11 => PixelFormat::D3D11,
            ffmpeg::format::Pixel::CUDA => PixelFormat::CUDA,
            ffmpeg::format::Pixel::QSV => PixelFormat::QSV,
            ffmpeg::format::Pixel::VULKAN => PixelFormat::VULKAN,
            // Planar GBR
            ffmpeg::format::Pixel::GBRP => PixelFormat::GBRP,
            ffmpeg::format::Pixel::GBRP10LE => PixelFormat::GBRP10LE,
            ffmpeg::format::Pixel::GBRP12LE => PixelFormat::GBRP12LE,
            // None
            ffmpeg::format::Pixel::None => PixelFormat::None,
            other => PixelFormat::Other(other.into()),
        }
    }
}

// ── ColorSpace ──────────────────────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorSpace {
    RGB,
    BT709,
    Unspecified,
    Reserved,
    FCC,
    BT470BG,
    SMPTE170M,
    SMPTE240M,
    YCGCO,
    BT2020NCL,
    BT2020CL,
    SMPTE2085,
    ChromaDerivedNCL,
    ChromaDerivedCL,
    ICTCP,
    Other(ffmpeg::ffi::AVColorSpace),
}

impl ColorSpace {
    /// Get a human-readable description of the color space
    pub fn description(&self) -> &'static str {
        match self {
            ColorSpace::RGB => "RGB",
            ColorSpace::BT709 => "BT.709",
            ColorSpace::Unspecified => "Unspecified",
            ColorSpace::Reserved => "Reserved",
            ColorSpace::FCC => "FCC",
            ColorSpace::BT470BG => "BT.470 BG",
            ColorSpace::SMPTE170M => "SMPTE-170M",
            ColorSpace::SMPTE240M => "SMPTE-240M",
            ColorSpace::YCGCO => "YCgCo",
            ColorSpace::BT2020NCL => "BT.2020 Non-Constant Luminance",
            ColorSpace::BT2020CL => "BT.2020 Constant Luminance",
            ColorSpace::SMPTE2085 => "SMPTE-2085",
            ColorSpace::ChromaDerivedNCL => "Chroma-Derived Non-Constant Luminance",
            ColorSpace::ChromaDerivedCL => "Chroma-Derived Constant Luminance",
            ColorSpace::ICTCP => "ICtCp",
            ColorSpace::Other(_) => "Unknown",
        }
    }
}

impl From<ffmpeg::color::Space> for ColorSpace {
    fn from(space: ffmpeg::color::Space) -> Self {
        match space {
            ffmpeg::color::Space::RGB => ColorSpace::RGB,
            ffmpeg::color::Space::BT709 => ColorSpace::BT709,
            ffmpeg::color::Space::Unspecified => ColorSpace::Unspecified,
            ffmpeg::color::Space::Reserved => ColorSpace::Reserved,
            ffmpeg::color::Space::FCC => ColorSpace::FCC,
            ffmpeg::color::Space::BT470BG => ColorSpace::BT470BG,
            ffmpeg::color::Space::SMPTE170M => ColorSpace::SMPTE170M,
            ffmpeg::color::Space::SMPTE240M => ColorSpace::SMPTE240M,
            ffmpeg::color::Space::YCGCO => ColorSpace::YCGCO,
            ffmpeg::color::Space::BT2020NCL => ColorSpace::BT2020NCL,
            ffmpeg::color::Space::BT2020CL => ColorSpace::BT2020CL,
            ffmpeg::color::Space::SMPTE2085 => ColorSpace::SMPTE2085,
            ffmpeg::color::Space::ChromaDerivedNCL => ColorSpace::ChromaDerivedNCL,
            ffmpeg::color::Space::ChromaDerivedCL => ColorSpace::ChromaDerivedCL,
            ffmpeg::color::Space::ICTCP => ColorSpace::ICTCP,
            other => ColorSpace::Other(other.into()),
        }
    }
}

// ── ColorRange ──────────────────────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorRange {
    MPEG,
    JPEG,
    Unspecified,
}

impl ColorRange {
    /// Get a human-readable description of the color range
    pub fn description(&self) -> &'static str {
        match self {
            ColorRange::MPEG => "Limited (TV)",
            ColorRange::JPEG => "Full (PC)",
            ColorRange::Unspecified => "Unspecified",
        }
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

// ── ColorPrimaries ──────────────────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorPrimaries {
    Reserved0,
    BT709,
    Unspecified,
    Reserved,
    BT470M,
    BT470BG,
    SMPTE170M,
    SMPTE240M,
    Film,
    BT2020,
    SMPTE428,
    SMPTE431,
    SMPTE432,
    EBU3213,
    Other(ffmpeg::ffi::AVColorPrimaries),
}

impl ColorPrimaries {
    /// Get a human-readable description of the color primaries
    pub fn description(&self) -> &'static str {
        match self {
            ColorPrimaries::Reserved0 => "Reserved",
            ColorPrimaries::BT709 => "BT.709",
            ColorPrimaries::Unspecified => "Unspecified",
            ColorPrimaries::Reserved => "Reserved",
            ColorPrimaries::BT470M => "BT.470M",
            ColorPrimaries::BT470BG => "BT.470 BG",
            ColorPrimaries::SMPTE170M => "SMPTE-170M",
            ColorPrimaries::SMPTE240M => "SMPTE-240M",
            ColorPrimaries::Film => "Film",
            ColorPrimaries::BT2020 => "BT.2020",
            ColorPrimaries::SMPTE428 => "SMPTE-428",
            ColorPrimaries::SMPTE431 => "SMPTE-431 (DCI-P3)",
            ColorPrimaries::SMPTE432 => "SMPTE-432 (Display P3)",
            ColorPrimaries::EBU3213 => "EBU 3213-E",
            ColorPrimaries::Other(_) => "Unknown",
        }
    }
}

impl From<ffmpeg::color::Primaries> for ColorPrimaries {
    fn from(primaries: ffmpeg::color::Primaries) -> Self {
        match primaries {
            ffmpeg::color::Primaries::Reserved0 => ColorPrimaries::Reserved0,
            ffmpeg::color::Primaries::BT709 => ColorPrimaries::BT709,
            ffmpeg::color::Primaries::Unspecified => ColorPrimaries::Unspecified,
            ffmpeg::color::Primaries::Reserved => ColorPrimaries::Reserved,
            ffmpeg::color::Primaries::BT470M => ColorPrimaries::BT470M,
            ffmpeg::color::Primaries::BT470BG => ColorPrimaries::BT470BG,
            ffmpeg::color::Primaries::SMPTE170M => ColorPrimaries::SMPTE170M,
            ffmpeg::color::Primaries::SMPTE240M => ColorPrimaries::SMPTE240M,
            ffmpeg::color::Primaries::Film => ColorPrimaries::Film,
            ffmpeg::color::Primaries::BT2020 => ColorPrimaries::BT2020,
            ffmpeg::color::Primaries::SMPTE428 => ColorPrimaries::SMPTE428,
            ffmpeg::color::Primaries::SMPTE431 => ColorPrimaries::SMPTE431,
            ffmpeg::color::Primaries::SMPTE432 => ColorPrimaries::SMPTE432,
            ffmpeg::color::Primaries::EBU3213 => ColorPrimaries::EBU3213,
            // Keep catch-all for forward compatibility with new ffmpeg-next versions
            #[allow(unreachable_patterns)]
            other => ColorPrimaries::Other(other.into()),
        }
    }
}

// ── ColorTransferCharacteristic ─────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ColorTransferCharacteristic {
    Reserved0,
    BT709,
    Unspecified,
    Reserved,
    GAMMA22,
    GAMMA28,
    SMPTE170M,
    SMPTE240M,
    Linear,
    Log,
    LogSqrt,
    #[allow(non_camel_case_types)]
    IEC61966_2_4,
    #[allow(non_camel_case_types)]
    BT1361_ECG,
    #[allow(non_camel_case_types)]
    IEC61966_2_1,
    #[allow(non_camel_case_types)]
    BT2020_10,
    #[allow(non_camel_case_types)]
    BT2020_12,
    SMPTE2084,
    SMPTE428,
    AribStdB67,
    Other(ffmpeg::ffi::AVColorTransferCharacteristic),
}

impl ColorTransferCharacteristic {
    /// Get a human-readable description of the transfer characteristic
    pub fn description(&self) -> &'static str {
        match self {
            ColorTransferCharacteristic::Reserved0 => "Reserved",
            ColorTransferCharacteristic::BT709 => "BT.709",
            ColorTransferCharacteristic::Unspecified => "Unspecified",
            ColorTransferCharacteristic::Reserved => "Reserved",
            ColorTransferCharacteristic::GAMMA22 => "Gamma 2.2",
            ColorTransferCharacteristic::GAMMA28 => "Gamma 2.8",
            ColorTransferCharacteristic::SMPTE170M => "SMPTE-170M",
            ColorTransferCharacteristic::SMPTE240M => "SMPTE-240M",
            ColorTransferCharacteristic::Linear => "Linear",
            ColorTransferCharacteristic::Log => "Logarithmic (100:1)",
            ColorTransferCharacteristic::LogSqrt => "Logarithmic (316:1)",
            ColorTransferCharacteristic::IEC61966_2_4 => "IEC 61966-2-4",
            ColorTransferCharacteristic::BT1361_ECG => "BT.1361 Extended Colour Gamut",
            ColorTransferCharacteristic::IEC61966_2_1 => "IEC 61966-2-1 (sRGB/sYCC)",
            ColorTransferCharacteristic::BT2020_10 => "BT.2020 10-bit",
            ColorTransferCharacteristic::BT2020_12 => "BT.2020 12-bit",
            ColorTransferCharacteristic::SMPTE2084 => "SMPTE-2084 (PQ)",
            ColorTransferCharacteristic::SMPTE428 => "SMPTE-428",
            ColorTransferCharacteristic::AribStdB67 => "HLG (Hybrid Log-Gamma)",
            ColorTransferCharacteristic::Other(_) => "Unknown",
        }
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
            ffmpeg::color::TransferCharacteristic::Reserved0 => {
                ColorTransferCharacteristic::Reserved0
            }
            ffmpeg::color::TransferCharacteristic::BT709 => ColorTransferCharacteristic::BT709,
            ffmpeg::color::TransferCharacteristic::Unspecified => {
                ColorTransferCharacteristic::Unspecified
            }
            ffmpeg::color::TransferCharacteristic::Reserved => {
                ColorTransferCharacteristic::Reserved
            }
            ffmpeg::color::TransferCharacteristic::GAMMA22 => ColorTransferCharacteristic::GAMMA22,
            ffmpeg::color::TransferCharacteristic::GAMMA28 => ColorTransferCharacteristic::GAMMA28,
            ffmpeg::color::TransferCharacteristic::SMPTE170M => {
                ColorTransferCharacteristic::SMPTE170M
            }
            ffmpeg::color::TransferCharacteristic::SMPTE240M => {
                ColorTransferCharacteristic::SMPTE240M
            }
            ffmpeg::color::TransferCharacteristic::Linear => ColorTransferCharacteristic::Linear,
            ffmpeg::color::TransferCharacteristic::Log => ColorTransferCharacteristic::Log,
            ffmpeg::color::TransferCharacteristic::LogSqrt => ColorTransferCharacteristic::LogSqrt,
            ffmpeg::color::TransferCharacteristic::IEC61966_2_4 => {
                ColorTransferCharacteristic::IEC61966_2_4
            }
            ffmpeg::color::TransferCharacteristic::BT1361_ECG => {
                ColorTransferCharacteristic::BT1361_ECG
            }
            ffmpeg::color::TransferCharacteristic::IEC61966_2_1 => {
                ColorTransferCharacteristic::IEC61966_2_1
            }
            ffmpeg::color::TransferCharacteristic::BT2020_10 => {
                ColorTransferCharacteristic::BT2020_10
            }
            ffmpeg::color::TransferCharacteristic::BT2020_12 => {
                ColorTransferCharacteristic::BT2020_12
            }
            ffmpeg::color::TransferCharacteristic::SMPTE2084 => {
                ColorTransferCharacteristic::SMPTE2084
            }
            ffmpeg::color::TransferCharacteristic::SMPTE428 => {
                ColorTransferCharacteristic::SMPTE428
            }
            ffmpeg::color::TransferCharacteristic::ARIB_STD_B67 => {
                ColorTransferCharacteristic::AribStdB67
            }
            // Keep catch-all for forward compatibility with new ffmpeg-next versions
            #[allow(unreachable_patterns)]
            other => ColorTransferCharacteristic::Other(other.into()),
        }
    }
}

// ── ChromaLocation ──────────────────────────────────────────────────────────

#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash)]
pub enum ChromaLocation {
    Left,
    Center,
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
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
            ChromaLocation::Unspecified => "Unspecified",
        }
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
