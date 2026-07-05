use std::os::raw::c_char;

use ffmpeg_next::{
    self as ffmpeg,
    ffi::{AVChannelLayout, av_channel_layout_describe},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Get aspect ratio as a float
    pub fn aspect_ratio(&self) -> Option<f32> {
        if self.height == 0 {
            return None;
        }
        Some(self.width as f32 / self.height as f32)
    }
}

impl std::fmt::Debug for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    U8(SampleType),
    I16(SampleType),
    I32(SampleType),
    I64(SampleType),
    F32(SampleType),
    F64(SampleType),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleType {
    Packed,
    Planar,
}

impl SampleFormat {
    /// Get bit depth from sample format
    pub fn bit_depth(&self) -> Option<u8> {
        match self {
            SampleFormat::U8(_) => Some(8),
            SampleFormat::I16(_) => Some(16),
            SampleFormat::I32(_) => Some(32),
            SampleFormat::I64(_) => Some(64),
            SampleFormat::F32(_) => Some(32),
            SampleFormat::F64(_) => Some(64),
            SampleFormat::None => None,
        }
    }

    /// Check if the sample format is planar
    pub fn is_planar(&self) -> bool {
        matches!(
            self,
            SampleFormat::U8(SampleType::Planar)
                | SampleFormat::I16(SampleType::Planar)
                | SampleFormat::I32(SampleType::Planar)
                | SampleFormat::I64(SampleType::Planar)
                | SampleFormat::F32(SampleType::Planar)
                | SampleFormat::F64(SampleType::Planar)
        )
    }

    /// Get a human-readable description of the sample format
    pub fn description(&self) -> &'static str {
        match self {
            SampleFormat::U8(SampleType::Packed) => "8-bit unsigned",
            SampleFormat::U8(SampleType::Planar) => "8-bit unsigned planar",
            SampleFormat::I16(SampleType::Packed) => "16-bit signed",
            SampleFormat::I16(SampleType::Planar) => "16-bit signed planar",
            SampleFormat::I32(SampleType::Packed) => "32-bit signed",
            SampleFormat::I32(SampleType::Planar) => "32-bit signed planar",
            SampleFormat::I64(SampleType::Packed) => "64-bit signed",
            SampleFormat::I64(SampleType::Planar) => "64-bit signed planar",
            SampleFormat::F32(SampleType::Packed) => "32-bit float",
            SampleFormat::F32(SampleType::Planar) => "32-bit float planar",
            SampleFormat::F64(SampleType::Packed) => "64-bit float",
            SampleFormat::F64(SampleType::Planar) => "64-bit float planar",
            SampleFormat::None => "None",
        }
    }
}

impl From<ffmpeg::format::Sample> for SampleFormat {
    fn from(sample: ffmpeg::format::Sample) -> Self {
        match sample {
            ffmpeg::format::Sample::U8(t) => SampleFormat::U8(t.into()),
            ffmpeg::format::Sample::I16(t) => SampleFormat::I16(t.into()),
            ffmpeg::format::Sample::I32(t) => SampleFormat::I32(t.into()),
            ffmpeg::format::Sample::I64(t) => SampleFormat::I64(t.into()),
            ffmpeg::format::Sample::F32(t) => SampleFormat::F32(t.into()),
            ffmpeg::format::Sample::F64(t) => SampleFormat::F64(t.into()),
            ffmpeg::format::Sample::None => SampleFormat::None,
        }
    }
}

impl From<ffmpeg::format::sample::Type> for SampleType {
    fn from(t: ffmpeg::format::sample::Type) -> Self {
        match t {
            ffmpeg::format::sample::Type::Packed => SampleType::Packed,
            ffmpeg::format::sample::Type::Planar => SampleType::Planar,
        }
    }
}

#[derive(Eq, PartialEq, Clone, Debug)]
pub struct ChannelLayout {
    pub channels: u16,
    pub description: Option<String>,
}

impl ChannelLayout {
    /// Get the number of channels
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Get a string description of the channel layout
    pub fn description(&self) -> Option<String> {
        self.description.clone()
    }

    /// Check if this is a standard surround sound layout
    pub fn is_surround(&self) -> bool {
        self.channels > 2
    }
}

impl From<ffmpeg::channel_layout::ChannelLayout> for ChannelLayout {
    fn from(layout: ffmpeg::channel_layout::ChannelLayout) -> Self {
        let channels = layout.channels().try_into().unwrap_or(0);
        let description = unsafe {
            let mut buf = vec![0u8; 128];
            let ret = av_channel_layout_describe(
                &layout.0 as *const AVChannelLayout,
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
            );

            if ret < 0 {
                None
            } else {
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                String::from_utf8(buf[..len].to_vec()).ok()
            }
        };

        ChannelLayout {
            channels,
            description,
        }
    }
}

/// Represents [ffmpeg::format::stream::Disposition]
#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash, Default)]
pub struct Disposition {
    flags: i32,
}

impl Disposition {
    /// Check if this stream is the default stream
    pub fn is_default(&self) -> bool {
        (self.flags & ffmpeg::format::stream::Disposition::DEFAULT.bits()) != 0
    }

    /// Check if this stream is forced
    pub fn is_forced(&self) -> bool {
        (self.flags & ffmpeg::format::stream::Disposition::FORCED.bits()) != 0
    }

    /// Check if this stream contains hearing impaired content
    pub fn is_hearing_impaired(&self) -> bool {
        (self.flags & ffmpeg::format::stream::Disposition::HEARING_IMPAIRED.bits()) != 0
    }

    /// Check if this stream contains visual impaired content
    pub fn is_visual_impaired(&self) -> bool {
        (self.flags & ffmpeg::format::stream::Disposition::VISUAL_IMPAIRED.bits()) != 0
    }

    /// Get a human-readable description of the disposition flags
    pub fn description(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();

        if self.is_default() {
            flags.push("Default");
        }
        if self.is_forced() {
            flags.push("Forced");
        }
        if self.is_hearing_impaired() {
            flags.push("Hearing Impaired");
        }
        if self.is_visual_impaired() {
            flags.push("Visual Impaired");
        }

        flags
    }
}

impl From<ffmpeg::format::stream::Disposition> for Disposition {
    fn from(disposition: ffmpeg::format::stream::Disposition) -> Self {
        Disposition {
            flags: disposition.bits(),
        }
    }
}
