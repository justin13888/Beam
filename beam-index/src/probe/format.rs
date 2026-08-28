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
        let mut buf = vec![0u8; 128];
        let ret = unsafe {
            av_channel_layout_describe(
                &layout.0 as *const AVChannelLayout,
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
            )
        };
        let description = describe_from_buffer(ret, &buf);

        ChannelLayout {
            channels,
            description,
        }
    }
}

/// Turn `av_channel_layout_describe`'s return code and output buffer into a
/// name.
///
/// Separated from the `unsafe` call so both halves are testable: FFmpeg
/// describes every layout it can represent, including "0 channels", so the
/// failure branch is unreachable through the public conversion -- but it is
/// the branch that decides whether a garbage buffer gets decoded, and dropping
/// it would be a silent correctness hole the day FFmpeg starts returning one.
fn describe_from_buffer(ret: std::os::raw::c_int, buf: &[u8]) -> Option<String> {
    if ret < 0 {
        return None;
    }
    // The buffer is NUL-terminated; anything past the terminator is
    // uninitialised padding and must not become part of the name.
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

/// Represents [ffmpeg::format::stream::Disposition]
#[derive(Eq, PartialEq, Copy, Clone, Debug, Hash, Default)]
pub struct Disposition {
    flags: i32,
}

impl Disposition {
    /// Build a `Disposition` with just the default/forced flags set, for
    /// tests that don't need a real ffmpeg-probed stream.
    #[cfg(test)]
    pub(crate) fn for_test(default: bool, forced: bool) -> Self {
        let mut flags = 0;
        if default {
            flags |= ffmpeg::format::stream::Disposition::DEFAULT.bits();
        }
        if forced {
            flags |= ffmpeg::format::stream::Disposition::FORCED.bits();
        }
        Self { flags }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    mod resolution {
        use super::*;

        #[test]
        fn aspect_ratio_is_width_over_height() {
            assert_eq!(Resolution::new(1920, 1080).aspect_ratio(), Some(16.0 / 9.0));
            assert_eq!(Resolution::new(640, 480).aspect_ratio(), Some(4.0 / 3.0));
        }

        #[test]
        fn a_zero_height_has_no_aspect_ratio_rather_than_an_infinity() {
            // Probing a corrupt or audio-only stream can yield a 0-height
            // "video" resolution; dividing by it would poison every downstream
            // comparison with NaN/inf instead of failing visibly.
            assert_eq!(Resolution::new(1920, 0).aspect_ratio(), None);
            assert_eq!(Resolution::new(0, 0).aspect_ratio(), None);
        }

        #[test]
        fn the_debug_format_is_the_conventional_wxh() {
            // It goes into log lines and admin output, where `1920x1080` is
            // what an operator expects to see.
            assert_eq!(format!("{:?}", Resolution::new(1920, 1080)), "1920x1080");
        }
    }

    mod sample_format {
        use super::*;

        /// Every variant, so a new one cannot be added without deciding what
        /// its depth, planarity, and description are.
        const ALL: &[SampleFormat] = &[
            SampleFormat::U8(SampleType::Packed),
            SampleFormat::U8(SampleType::Planar),
            SampleFormat::I16(SampleType::Packed),
            SampleFormat::I16(SampleType::Planar),
            SampleFormat::I32(SampleType::Packed),
            SampleFormat::I32(SampleType::Planar),
            SampleFormat::I64(SampleType::Packed),
            SampleFormat::I64(SampleType::Planar),
            SampleFormat::F32(SampleType::Packed),
            SampleFormat::F32(SampleType::Planar),
            SampleFormat::F64(SampleType::Packed),
            SampleFormat::F64(SampleType::Planar),
            SampleFormat::None,
        ];

        #[test]
        fn bit_depth_matches_the_name_of_the_variant() {
            for format in ALL {
                let expected = match format {
                    SampleFormat::U8(_) => Some(8),
                    SampleFormat::I16(_) => Some(16),
                    SampleFormat::I32(_) | SampleFormat::F32(_) => Some(32),
                    SampleFormat::I64(_) | SampleFormat::F64(_) => Some(64),
                    SampleFormat::None => None,
                };
                assert_eq!(format.bit_depth(), expected, "for {format:?}");
            }
        }

        #[test]
        fn planarity_follows_the_sample_type_and_nothing_else() {
            for format in ALL {
                let expected = matches!(
                    format,
                    SampleFormat::U8(SampleType::Planar)
                        | SampleFormat::I16(SampleType::Planar)
                        | SampleFormat::I32(SampleType::Planar)
                        | SampleFormat::I64(SampleType::Planar)
                        | SampleFormat::F32(SampleType::Planar)
                        | SampleFormat::F64(SampleType::Planar)
                );
                assert_eq!(format.is_planar(), expected, "for {format:?}");
            }
        }

        #[test]
        fn every_variant_has_its_own_description() {
            let mut seen = std::collections::HashSet::new();
            for format in ALL {
                assert!(
                    seen.insert(format.description()),
                    "{format:?} shares a description with another variant"
                );
            }
            // Planar variants say so; packed ones do not.
            assert!(
                SampleFormat::F32(SampleType::Planar)
                    .description()
                    .ends_with("planar")
            );
            assert!(
                !SampleFormat::F32(SampleType::Packed)
                    .description()
                    .ends_with("planar")
            );
        }
    }

    mod channel_layout {
        use super::*;

        fn layout(channels: u16) -> ChannelLayout {
            ChannelLayout {
                channels,
                description: None,
            }
        }

        /// FFmpeg's own name for a standard layout, read back through the FFI
        /// `av_channel_layout_describe` call this module wraps.
        fn described(ffmpeg_layout: ffmpeg::channel_layout::ChannelLayout) -> ChannelLayout {
            crate::probe::init().expect("ffmpeg init");
            ChannelLayout::from(ffmpeg_layout)
        }

        #[test]
        fn a_standard_layout_is_described_by_name_not_left_blank() {
            // The description is what the UI shows for an audio track. The
            // FFI call writes a NUL-terminated string into a raw buffer; a
            // mishandled return code or terminator search yields either
            // nothing or a string full of trailing NULs.
            let stereo = described(ffmpeg::channel_layout::ChannelLayout::STEREO);
            assert_eq!(stereo.channels(), 2);
            assert_eq!(stereo.description().as_deref(), Some("stereo"));
            assert!(!stereo.is_surround());

            let surround = described(ffmpeg::channel_layout::ChannelLayout::_5POINT1);
            assert_eq!(surround.channels(), 6);
            let description = surround.description().expect("5.1 has a name");
            assert!(
                description.starts_with("5.1"),
                "expected FFmpeg's 5.1 name, got {description:?}"
            );
            assert!(surround.is_surround());
        }

        #[test]
        fn a_description_carries_no_trailing_nul_padding() {
            // The buffer is 128 bytes; the string ends at the first NUL, and
            // taking the whole buffer instead would produce a name that
            // compares unequal to itself everywhere it is displayed.
            let stereo = described(ffmpeg::channel_layout::ChannelLayout::STEREO);
            let description = stereo.description().expect("stereo has a name");
            assert!(!description.contains('\0'), "{description:?}");
            assert_eq!(description.trim(), description);
        }

        #[test]
        fn a_failed_describe_yields_no_name_rather_than_decoding_the_buffer() {
            // FFmpeg documents a negative return as an error; the buffer is
            // then uninitialised. Decoding it anyway would put arbitrary bytes
            // into a track label.
            assert_eq!(describe_from_buffer(-1, b"stereo\0garbage"), None);
            assert_eq!(describe_from_buffer(-22, &[0xff; 16]), None);
        }

        #[test]
        fn a_name_stops_at_the_nul_terminator() {
            assert_eq!(
                describe_from_buffer(6, b"stereo\0\0\0\0"),
                Some("stereo".to_string())
            );
            // No terminator at all: take the whole buffer rather than panic.
            assert_eq!(
                describe_from_buffer(6, b"stereo"),
                Some("stereo".to_string())
            );
            assert_eq!(describe_from_buffer(0, b"\0\0"), Some(String::new()));
        }

        #[test]
        fn a_name_that_is_not_utf8_is_dropped_rather_than_forced() {
            assert_eq!(describe_from_buffer(3, &[0xff, 0xfe, 0x00]), None);
        }

        #[test]
        fn mono_and_stereo_do_not_share_a_description() {
            let mono = described(ffmpeg::channel_layout::ChannelLayout::MONO);
            let stereo = described(ffmpeg::channel_layout::ChannelLayout::STEREO);
            assert_eq!(mono.channels(), 1);
            assert_ne!(mono.description(), stereo.description());
        }

        #[test]
        fn surround_starts_above_stereo() {
            // The boundary is what matters: stereo is not surround, 2.1 is.
            assert!(!layout(0).is_surround());
            assert!(!layout(1).is_surround(), "mono");
            assert!(!layout(2).is_surround(), "stereo");
            assert!(layout(3).is_surround(), "2.1");
            assert!(layout(6).is_surround(), "5.1");
            assert!(layout(8).is_surround(), "7.1");
        }
    }

    mod disposition {
        use super::*;

        #[test]
        fn flags_are_independent_of_one_another() {
            assert!(Disposition::for_test(true, false).is_default());
            assert!(!Disposition::for_test(true, false).is_forced());
            assert!(Disposition::for_test(false, true).is_forced());
            assert!(!Disposition::for_test(false, true).is_default());
            assert!(Disposition::for_test(true, true).is_default());
            assert!(Disposition::for_test(true, true).is_forced());
        }

        #[test]
        fn a_stream_with_no_flags_reports_nothing() {
            let plain = Disposition::for_test(false, false);
            assert!(!plain.is_default());
            assert!(!plain.is_forced());
            assert!(!plain.is_hearing_impaired());
            assert!(!plain.is_visual_impaired());
            assert!(plain.description().is_empty());
        }

        #[test]
        fn the_description_lists_the_flags_that_are_set() {
            assert_eq!(
                Disposition::for_test(true, true).description(),
                vec!["Default", "Forced"]
            );
            assert_eq!(
                Disposition::for_test(false, true).description(),
                vec!["Forced"]
            );
        }

        #[test]
        fn accessibility_flags_survive_the_round_trip_from_ffmpeg() {
            // These two drive subtitle-track selection for users who need
            // them, and nothing else in the suite exercises the conversion.
            use ffmpeg_next::format::stream::Disposition as FfDisposition;

            let hearing: Disposition = FfDisposition::HEARING_IMPAIRED.into();
            assert!(hearing.is_hearing_impaired());
            assert!(!hearing.is_visual_impaired());
            assert_eq!(hearing.description(), vec!["Hearing Impaired"]);

            let visual: Disposition = FfDisposition::VISUAL_IMPAIRED.into();
            assert!(visual.is_visual_impaired());
            assert_eq!(visual.description(), vec!["Visual Impaired"]);
        }

        #[test]
        fn a_default_forced_stream_round_trips_from_ffmpeg_flags() {
            use ffmpeg_next::format::stream::Disposition as FfDisposition;

            let both: Disposition = (FfDisposition::DEFAULT | FfDisposition::FORCED).into();
            assert_eq!(both.description(), vec!["Default", "Forced"]);
        }
    }
}
