use ffmpeg_next as ffmpeg;
use num::rational::Ratio;
use num::traits::cast::ToPrimitive;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tracing::trace;

use crate::probe::{
    color::{
        ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
        PixelFormat,
    },
    format::{ChannelLayout, Disposition, Resolution, SampleFormat},
    media::{CodecId, Discard},
};

pub type Rational = Ratio<i32>;

// Convert ffmpeg::Rational to our Rational type
// Returns Some(r) if valid, otherwise tuple (numer, denom).
fn into_rational(r: ffmpeg::Rational) -> Result<Rational, (i32, i32)> {
    let numer: i32 = r.0;
    let denom: i32 = r.1;

    if denom == 0 {
        return Err((numer, denom));
    }

    Ok(Ratio::new(numer, denom))
}

fn parse_duration_string(duration_str: &str) -> Option<f64> {
    // Parse duration strings like "00:45:23.000000000"
    let parts: Vec<&str> = duration_str.split(':').collect();
    if parts.len() == 3
        && let (Ok(hours), Ok(minutes), Ok(seconds)) = (
            parts[0].parse::<f64>(),
            parts[1].parse::<f64>(),
            parts[2].parse::<f64>(),
        )
    {
        return Some(hours * 3600.0 + minutes * 60.0 + seconds);
    }
    None
}

#[derive(Clone, Debug)]
pub struct VideoMetadata {
    pub bit_rate: u64,
    pub max_rate: u64,
    pub delay: usize,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub has_b_frames: bool,
    pub aspect_ratio: Rational,
    pub color_space: ColorSpace,
    pub color_range: ColorRange,
    pub color_primaries: ColorPrimaries,
    pub color_transfer_characteristic: ColorTransferCharacteristic,
    pub chroma_location: ChromaLocation,
    pub references: usize,
    pub intra_dc_precision: u8,
    pub profile: String,
    pub level: String,
    pub codec_name: String,
}

impl VideoMetadata {
    /// Get the actual bitrate, using metadata fallback if the primary bitrate is 0
    pub fn actual_bit_rate(&self, stream_metadata: &HashMap<String, String>) -> f64 {
        if self.bit_rate > 0 {
            self.bit_rate as f64
        } else if let Some(bps_str) = stream_metadata.get("BPS") {
            bps_str.parse::<f64>().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Get bit depth from pixel format
    /// Returns None if unknown.
    pub fn bit_depth(&self) -> Option<u8> {
        self.format.bit_depth()
    }

    /// Get resolution
    pub fn resolution(&self) -> Resolution {
        Resolution::new(self.width, self.height)
    }
}

#[derive(Clone, Debug)]
pub struct AudioMetadata {
    pub bit_rate: u64,
    pub max_rate: u64,
    pub delay: usize,
    pub rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    pub frames: usize,
    pub align: usize,
    pub channel_layout: ChannelLayout,
    pub codec_name: String,
    pub profile: String,
    pub title: String,
    pub language: String,
}

impl AudioMetadata {
    /// Get the actual bitrate, using metadata fallback if the primary bitrate is 0
    pub fn actual_bit_rate(&self, stream_metadata: &HashMap<String, String>) -> f64 {
        if self.bit_rate > 0 {
            self.bit_rate as f64
        } else if let Some(bps_str) = stream_metadata.get("BPS") {
            bps_str.parse::<f64>().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Get the actual frame count, using metadata fallback if frames is 0
    pub fn actual_frames(&self, stream_metadata: &HashMap<String, String>) -> i64 {
        if self.frames > 0 {
            self.frames as i64
        } else if let Some(frames_str) = stream_metadata.get("NUMBER_OF_FRAMES") {
            frames_str.parse::<i64>().unwrap_or(0)
        } else {
            0
        }
    }

    /// Get a human-readable description of the channel layout
    pub fn channel_layout_description(&self) -> &'static str {
        match self.channels {
            1 => "Mono",
            2 => "Stereo",
            6 => "5.1",
            8 => "7.1",
            _ => "Multi-channel",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VideoStreamMetadata {
    pub index: usize,
    pub time_base: Rational,
    pub start_time: i64,
    pub duration: i64,
    pub frames: i64,
    pub disposition: Disposition,
    pub discard: Discard,
    /// Base stream rate, if could be reliably determined
    pub rate: Option<Rational>,
    pub codec_id: CodecId,
    pub video: VideoMetadata,
    pub metadata: HashMap<String, String>,
}

impl VideoStreamMetadata {
    /// Compute duration in seconds from duration and time_base
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64().unwrap()
    }

    /// Compute frame rate from the stream rate
    pub fn frame_rate(&self) -> Option<f64> {
        self.rate.and_then(|r| r.to_f64())
    }

    /// Get the actual duration, using metadata fallback if duration is 0
    pub fn actual_duration_seconds(&self, file_duration_seconds: f64) -> f64 {
        if self.duration_seconds() > 0.0 {
            self.duration_seconds()
        } else {
            // Try to get duration from metadata or fall back to file duration
            if let Some(duration_str) = self.metadata.get("DURATION") {
                parse_duration_string(duration_str).unwrap_or(file_duration_seconds)
            } else {
                file_duration_seconds
            }
        }
    }

    /// Get the actual frame count, using metadata fallback if frames is 0
    pub fn actual_frames(&self) -> i64 {
        if self.frames > 0 {
            self.frames
        } else {
            // Try to get frame count from metadata
            if let Some(frames_str) = self.metadata.get("NUMBER_OF_FRAMES") {
                frames_str.parse::<i64>().unwrap_or(0)
            } else {
                0
            }
        }
    }

    /// Get a unique identifier based on codec and resolution
    pub fn unique_id(&self) -> String {
        format!(
            "{}-{}x{}",
            self.video.codec_name, self.video.width, self.video.height
        ) // TODO: make this a hash of all relevant properties
    }
}

#[derive(Clone, Debug)]
pub struct AudioStreamMetadata {
    pub index: usize,
    pub time_base: Rational,
    pub start_time: i64,
    pub duration: i64,
    pub frames: i64,
    pub disposition: Disposition,
    pub discard: Discard,
    /// Base stream rate, if could be reliably determined
    pub rate: Option<Rational>,
    pub codec_id: CodecId,
    pub audio: AudioMetadata,
    pub metadata: HashMap<String, String>,
}

impl AudioStreamMetadata {
    /// Compute duration in seconds from duration and time_base
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64().unwrap()
    }

    /// Get the actual duration, using metadata fallback if duration is 0
    pub fn actual_duration_seconds(&self, file_duration_seconds: f64) -> f64 {
        if self.duration_seconds() > 0.0 {
            self.duration_seconds()
        } else {
            // Try to get duration from metadata or fall back to file duration
            if let Some(duration_str) = self.metadata.get("DURATION") {
                parse_duration_string(duration_str).unwrap_or(file_duration_seconds)
            } else {
                file_duration_seconds
            }
        }
    }

    /// Get the actual frame count, using metadata fallback if frames is 0
    pub fn actual_frames(&self) -> i64 {
        if self.frames > 0 {
            self.frames
        } else {
            // Try to get frame count from metadata
            if let Some(frames_str) = self.metadata.get("NUMBER_OF_FRAMES") {
                frames_str.parse::<i64>().unwrap_or(0)
            } else {
                0
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubtitleStreamMetadata {
    pub index: usize,
    pub time_base: Rational,
    pub start_time: i64,
    pub duration: i64,
    pub disposition: Disposition,
    pub discard: Discard,
    pub codec_id: CodecId,
    pub metadata: HashMap<String, String>,
}

impl SubtitleStreamMetadata {
    /// Compute duration in seconds from duration and time_base
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64().unwrap()
    }

    /// Get the actual duration, using metadata fallback if duration is 0
    pub fn actual_duration_seconds(&self, file_duration_seconds: f64) -> f64 {
        if self.duration_seconds() > 0.0 {
            self.duration_seconds()
        } else {
            // Try to get duration from metadata or fall back to file duration
            if let Some(duration_str) = self.metadata.get("DURATION") {
                parse_duration_string(duration_str).unwrap_or(file_duration_seconds)
            } else {
                file_duration_seconds
            }
        }
    }

    /// Get title from metadata if available
    /// Returns empty string if not present.
    pub fn title(&self) -> Option<String> {
        self.metadata.get("title").cloned()
    }

    /// Get language from metadata if available
    /// Returns empty string if not present.
    pub fn language(&self) -> Option<String> {
        self.metadata.get("language").cloned()
    }
}

/// Stream metadata encapsulating various supported stream types
#[derive(Clone, Debug)]
pub enum StreamMetadata {
    Video(VideoStreamMetadata),
    Audio(AudioStreamMetadata),
    Subtitle(SubtitleStreamMetadata),
}

impl StreamMetadata {
    /// Get the stream index
    pub fn index(&self) -> usize {
        match self {
            StreamMetadata::Video(v) => v.index,
            StreamMetadata::Audio(a) => a.index,
            StreamMetadata::Subtitle(s) => s.index,
        }
    }

    /// Get the stream time_base
    pub fn time_base(&self) -> Rational {
        match self {
            StreamMetadata::Video(v) => v.time_base,
            StreamMetadata::Audio(a) => a.time_base,
            StreamMetadata::Subtitle(s) => s.time_base,
        }
    }

    /// Get the stream metadata
    pub fn metadata(&self) -> &HashMap<String, String> {
        match self {
            StreamMetadata::Video(v) => &v.metadata,
            StreamMetadata::Audio(a) => &a.metadata,
            StreamMetadata::Subtitle(s) => &s.metadata,
        }
    }

    /// Compute duration in seconds from duration and time_base
    pub fn duration_seconds(&self) -> f64 {
        match self {
            StreamMetadata::Video(v) => v.duration_seconds(),
            StreamMetadata::Audio(a) => a.duration_seconds(),
            StreamMetadata::Subtitle(s) => s.duration_seconds(),
        }
    }

    /// Get the actual duration, using metadata fallback if duration is 0
    pub fn actual_duration_seconds(&self, file_duration_seconds: f64) -> f64 {
        match self {
            StreamMetadata::Video(v) => v.actual_duration_seconds(file_duration_seconds),
            StreamMetadata::Audio(a) => a.actual_duration_seconds(file_duration_seconds),
            StreamMetadata::Subtitle(s) => s.actual_duration_seconds(file_duration_seconds),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VideoFileMetadata {
    /// File path to video file
    pub file_path: PathBuf,
    /// Key-value pairs of file-level metadata tags (e.g., title, artist, album)
    pub metadata: HashMap<String, String>,
    /// Index of the best/primary video stream, if any exists
    pub best_video_stream: Option<usize>,
    /// Index of the best/primary audio stream, if any exists
    pub best_audio_stream: Option<usize>,
    /// Index of the best/primary subtitle stream, if any exists
    pub best_subtitle_stream: Option<usize>,
    /// Duration of the media file in AV_TIME_BASE units (1/AV_TIME_BASE seconds)
    pub duration: i64,
    /// Collection of all streams (video, audio, subtitle, etc.) in the file
    pub streams: Vec<StreamMetadata>,
    /// Short name of the container format (e.g., "mp4", "mkv", "avi")
    pub format_name: String,
    /// Human-readable description of the container format
    pub format_long_name: String,
    /// Size of the file in bytes
    pub file_size: u64,
    /// Overall bitrate of the file in bits per second
    pub bit_rate: i64,
    /// Probe score indicating confidence in format detection (0-100)
    pub probe_score: i32,
}

/// FFmpeg's descriptive name for a container, falling back to its short name.
///
/// `ffmpeg_next::format::format::Input::description` is unsound: it hands
/// `AVInputFormat::long_name` straight to `CStr::from_ptr` with no null check.
/// That field is NULL in any `CONFIG_SMALL` build of FFmpeg -- what
/// `--enable-small` produces, and what distributions and slim container images
/// commonly ship -- so calling it segfaults the process on the first file
/// probed. Not an error, not a panic: a `strlen` on a null pointer.
///
/// Read the pointer and check it instead. `name` is a required field and is
/// always populated, which makes it the right fallback.
fn format_long_name(format: &ffmpeg::format::format::Input) -> String {
    let long_name = unsafe { (*format.as_ptr()).long_name };
    if long_name.is_null() {
        return format.name().to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(long_name) }
        .to_string_lossy()
        .into_owned()
}

impl VideoFileMetadata {
    // TODO: See if this should be async anyways vv
    /// From file path
    pub fn from_path(file_path: &Path) -> Result<Self, MetadataError> {
        trace!("Opening file for metadata extraction: {:?}", file_path);
        let context = ffmpeg::format::input(file_path)?;

        // Collect file-level metadata
        let metadata: HashMap<String, String> = context
            .metadata()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        // Find best streams
        let mut best_video_stream = context
            .streams()
            .best(ffmpeg::media::Type::Video)
            .map(|s| s.index());
        let mut best_audio_stream = context
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .map(|s| s.index());
        let mut best_subtitle_stream = context
            .streams()
            .best(ffmpeg::media::Type::Subtitle)
            .map(|s| s.index());

        // Get duration in AV_TIME_BASE units
        let duration = context.duration();

        // Process all streams
        trace!("Processing streams for file: {:?}", file_path);
        let mut streams: Vec<StreamMetadata> = vec![];
        for (i, stream) in context.streams().enumerate() {
            let codec =
                ffmpeg::codec::context::Context::from_parameters(stream.parameters()).unwrap();
            let medium = codec.medium();
            let codec_id = codec.id();
            trace!(medium = ?medium, codec_id = ?codec_id, "Processing stream index {i}");

            let metadata: HashMap<String, String> = stream
                .metadata()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            let stream_metadata: Option<StreamMetadata> = match medium {
                ffmpeg::media::Type::Video => {
                    trace!("Processing video stream index {}", stream.index());
                    let video_decoder = codec.decoder().video()?;
                    let codec_name = format!("{:?}", codec_id);
                    let profile = format!("{:?}", video_decoder.profile());
                    let level = "Unknown".to_string(); // Level not directly available in ffmpeg-next

                    let video = VideoMetadata {
                        bit_rate: video_decoder.bit_rate() as u64,
                        max_rate: video_decoder.max_bit_rate() as u64,
                        delay: video_decoder.delay(),
                        width: video_decoder.width(),
                        height: video_decoder.height(),
                        format: video_decoder.format().into(),
                        has_b_frames: video_decoder.has_b_frames(),
                        aspect_ratio: into_rational(video_decoder.aspect_ratio()).map_err(
                            |(n, d)| {
                                MetadataError::InvalidMetadata(format!(
                                    "Invalid aspect ratio {}/{} in stream {}",
                                    n,
                                    d,
                                    stream.index()
                                ))
                            },
                        )?,
                        color_space: video_decoder.color_space().into(),
                        color_range: video_decoder.color_range().into(),
                        color_primaries: video_decoder.color_primaries().into(),
                        color_transfer_characteristic: video_decoder
                            .color_transfer_characteristic()
                            .into(),
                        chroma_location: video_decoder.chroma_location().into(),
                        references: video_decoder.references(),
                        intra_dc_precision: video_decoder.intra_dc_precision(),
                        profile,
                        level,
                        codec_name,
                    };

                    let stream_metadata = StreamMetadata::Video(VideoStreamMetadata {
                        index: stream.index(),
                        time_base: into_rational(stream.time_base()).map_err(|(n, d)| {
                            MetadataError::InvalidMetadata(format!(
                                "Invalid time base {}/{} in stream {}",
                                n,
                                d,
                                stream.index()
                            ))
                        })?,
                        start_time: stream.start_time(),
                        duration: stream.duration(),
                        frames: stream.frames(),
                        disposition: stream.disposition().into(),
                        discard: stream.discard().into(),
                        rate: match into_rational(stream.rate()) {
                            Ok(r) => Ok(Some(r)),
                            Err((n, d)) => {
                                trace!(
                                    "Was unable to convert rate for stream {}: {}/{}",
                                    stream.index(),
                                    n,
                                    d
                                );
                                if n == 0 && d == 0 {
                                    Ok(None)
                                } else {
                                    Err(MetadataError::InvalidMetadata(format!(
                                        "Invalid rate {}/{} in stream {}",
                                        n,
                                        d,
                                        stream.index()
                                    )))
                                }
                            }
                        }?,
                        codec_id: codec_id.into(),
                        video,
                        metadata,
                    });

                    Ok::<Option<_>, MetadataError>(Some(stream_metadata))
                }
                ffmpeg::media::Type::Audio => {
                    trace!("Processing audio stream index {}", stream.index());
                    let audio_decoder = codec.decoder().audio()?;

                    let codec_name = format!("{:?}", codec_id);
                    let profile = format!("{:?}", audio_decoder.profile());

                    let mut title = String::new();
                    let mut language = String::new();

                    for (k, v) in stream.metadata().iter() {
                        match k {
                            "title" => title = v.to_string(),
                            "language" => language = v.to_string(),
                            _ => {}
                        }
                    }

                    let audio = AudioMetadata {
                        bit_rate: audio_decoder.bit_rate() as u64,
                        max_rate: audio_decoder.max_bit_rate() as u64,
                        delay: audio_decoder.delay(),
                        rate: audio_decoder.rate(),
                        channels: audio_decoder.channels(),
                        format: audio_decoder.format().into(),
                        frames: audio_decoder.frames(),
                        align: audio_decoder.align(),
                        channel_layout: audio_decoder.channel_layout().into(),
                        codec_name,
                        profile,
                        title,
                        language,
                    };

                    let stream_metadata = StreamMetadata::Audio(AudioStreamMetadata {
                        index: stream.index(),
                        time_base: into_rational(stream.time_base()).map_err(|(n, d)| {
                            MetadataError::InvalidMetadata(format!(
                                "Invalid time base {}/{} in stream {}",
                                n,
                                d,
                                stream.index()
                            ))
                        })?,
                        start_time: stream.start_time(),
                        duration: stream.duration(),
                        frames: stream.frames(),
                        disposition: stream.disposition().into(),
                        discard: stream.discard().into(),
                        rate: match into_rational(stream.rate()) {
                            Ok(r) => Ok(Some(r)),
                            Err((n, d)) => {
                                trace!(
                                    "Was unable to convert rate for stream {}: {}/{}",
                                    stream.index(),
                                    n,
                                    d
                                );
                                if n == 0 && d == 0 {
                                    Ok(None)
                                } else {
                                    Err(MetadataError::InvalidMetadata(format!(
                                        "Invalid rate {}/{} in stream {}",
                                        n,
                                        d,
                                        stream.index()
                                    )))
                                }
                            }
                        }?,
                        codec_id: codec_id.into(),
                        audio,
                        metadata,
                    });

                    Ok::<Option<_>, MetadataError>(Some(stream_metadata))
                }
                ffmpeg::media::Type::Subtitle => {
                    trace!("Processing subtitle stream index {}", stream.index());
                    Ok(Some(StreamMetadata::Subtitle(SubtitleStreamMetadata {
                        index: stream.index(),
                        time_base: into_rational(stream.time_base()).map_err(|(n, d)| {
                            MetadataError::InvalidMetadata(format!(
                                "Invalid time base {}/{} in stream {}",
                                n,
                                d,
                                stream.index()
                            ))
                        })?,
                        start_time: stream.start_time(),
                        duration: stream.duration(),
                        disposition: stream.disposition().into(),
                        discard: stream.discard().into(),
                        codec_id: codec_id.into(),
                        metadata,
                    })))
                }
                ffmpeg::media::Type::Data
                | ffmpeg::media::Type::Attachment
                | ffmpeg::media::Type::Unknown => {
                    // Skip other stream types
                    Ok(None)
                }
            }?;

            if let Some(stream_metadata) = stream_metadata {
                let insertion_idx = streams.len();

                // Update best stream indices if not already set
                match &stream_metadata {
                    StreamMetadata::Video(_) => {
                        if let Some(idx) = best_video_stream
                            && i == idx
                        {
                            best_video_stream = Some(insertion_idx);
                        }
                    }
                    StreamMetadata::Audio(_) => {
                        if let Some(idx) = best_audio_stream
                            && i == idx
                        {
                            best_audio_stream = Some(insertion_idx);
                        }
                    }
                    StreamMetadata::Subtitle(_) => {
                        if let Some(idx) = best_subtitle_stream
                            && i == idx
                        {
                            best_subtitle_stream = Some(insertion_idx);
                        }
                    }
                }

                // Insert the stream metadata
                streams.push(stream_metadata);
            }
        }

        // Get format information
        let format_name = context.format().name().to_string();
        let format_long_name = format_long_name(&context.format());
        let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        let bit_rate = context.bit_rate();
        let probe_score = context.probe_score();

        Ok(VideoFileMetadata {
            file_path: file_path.to_path_buf(),
            metadata,
            best_video_stream,
            best_audio_stream,
            best_subtitle_stream,
            duration,
            streams,
            format_name,
            format_long_name,
            file_size,
            bit_rate,
            probe_score,
        })
    }

    /// Compute duration in seconds
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("FFmpeg error: {0}")]
    FfmpegError(#[from] ffmpeg::Error),
    #[error("Invalid metadata encountered: {0}")]
    InvalidMetadata(String),
    #[error("Unknown error: {0}")]
    UnknownError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::color::{
        ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
    };
    use crate::probe::format::Disposition;

    /// Matroska carries duration, bitrate, and frame counts as *tags* rather
    /// than in the stream header, so for a large share of a real library the
    /// header values are zero and these fallbacks are the only thing standing
    /// between the UI and a file that claims to be 0 seconds long. That is
    /// what the tests below are about.
    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn video_metadata(bit_rate: u64) -> VideoMetadata {
        VideoMetadata {
            bit_rate,
            max_rate: 0,
            delay: 0,
            width: 1920,
            height: 1080,
            format: PixelFormat::YUV420P,
            has_b_frames: false,
            aspect_ratio: Rational::new(1, 1),
            color_space: ColorSpace::BT709,
            color_range: ColorRange::MPEG,
            color_primaries: ColorPrimaries::BT709,
            color_transfer_characteristic: ColorTransferCharacteristic::BT709,
            chroma_location: ChromaLocation::Left,
            references: 0,
            intra_dc_precision: 0,
            codec_name: "h264".to_string(),
            profile: "High".to_string(),
            level: "4.0".to_string(),
        }
    }

    fn audio_metadata(bit_rate: u64, frames: usize, channels: u16) -> AudioMetadata {
        AudioMetadata {
            bit_rate,
            max_rate: 0,
            delay: 0,
            rate: 48_000,
            channels,
            format: SampleFormat::F32(crate::probe::format::SampleType::Planar),
            frames,
            align: 0,
            channel_layout: ChannelLayout {
                channels,
                description: None,
            },
            codec_name: "aac".to_string(),
            profile: "LC".to_string(),
            title: String::new(),
            language: String::new(),
        }
    }

    fn video_stream(
        duration: i64,
        frames: i64,
        metadata: HashMap<String, String>,
    ) -> VideoStreamMetadata {
        VideoStreamMetadata {
            index: 0,
            time_base: Rational::new(1, 1000),
            start_time: 0,
            duration,
            frames,
            rate: Some(Rational::new(24_000, 1001)),
            disposition: Disposition::default(),
            discard: Discard::Default,
            codec_id: CodecId::H264,
            video: video_metadata(0),
            metadata,
        }
    }

    fn subtitle_stream(duration: i64, metadata: HashMap<String, String>) -> SubtitleStreamMetadata {
        SubtitleStreamMetadata {
            index: 2,
            time_base: Rational::new(1, 1000),
            start_time: 0,
            duration,
            disposition: Disposition::default(),
            discard: Discard::Default,
            codec_id: CodecId::SUBRIP,
            metadata,
        }
    }

    mod rationals {
        use super::*;

        #[test]
        fn a_valid_rational_is_converted_and_reduced() {
            // Time bases and frame rates arrive as raw numerator/denominator
            // pairs; every duration in the probe is computed from them.
            assert_eq!(
                into_rational(ffmpeg::Rational(1, 1000)),
                Ok(Ratio::new(1, 1000))
            );
            assert_eq!(
                into_rational(ffmpeg::Rational(24_000, 1001)),
                Ok(Ratio::new(24_000, 1001))
            );
            // `Ratio::new` reduces, so 2/4 and 1/2 are the same rational.
            assert_eq!(into_rational(ffmpeg::Rational(2, 4)), Ok(Ratio::new(1, 2)));
        }

        #[test]
        fn a_zero_denominator_is_reported_rather_than_dividing_by_zero() {
            // A corrupt or still-probing stream can report 1/0. `Ratio::new`
            // panics on a zero denominator, and a silent `0/1` default would
            // make every duration computed from it zero.
            assert_eq!(into_rational(ffmpeg::Rational(1, 0)), Err((1, 0)));
            assert_eq!(into_rational(ffmpeg::Rational(0, 0)), Err((0, 0)));
        }

        #[test]
        fn a_zero_numerator_is_a_valid_rational() {
            assert_eq!(
                into_rational(ffmpeg::Rational(0, 1000)),
                Ok(Ratio::new(0, 1000))
            );
        }
    }

    mod duration_strings {
        use super::*;

        #[test]
        fn a_matroska_duration_tag_is_parsed_to_seconds() {
            assert_eq!(
                parse_duration_string("00:45:23.000000000"),
                Some(45.0 * 60.0 + 23.0)
            );
            assert_eq!(parse_duration_string("01:00:00.5"), Some(3600.5));
            assert_eq!(parse_duration_string("00:00:00.000000000"), Some(0.0));
        }

        #[test]
        fn hours_are_not_capped_at_a_day() {
            // A concatenated recording can legitimately exceed 24 hours.
            assert_eq!(parse_duration_string("30:00:00.0"), Some(30.0 * 3600.0));
        }

        #[test]
        fn anything_that_is_not_three_colon_separated_numbers_is_rejected() {
            for bad in [
                "",
                "45:23",
                "00:45:23:00",
                "abc",
                "00:xx:23.0",
                "00:45:xx",
                "::",
            ] {
                assert_eq!(parse_duration_string(bad), None, "for {bad:?}");
            }
        }
    }

    mod bit_rate_fallback {
        use super::*;

        #[test]
        fn the_header_bitrate_wins_when_it_is_present() {
            let stream_tags = tags(&[("BPS", "999")]);
            assert_eq!(video_metadata(5_000).actual_bit_rate(&stream_tags), 5_000.0);
            assert_eq!(
                audio_metadata(320_000, 0, 2).actual_bit_rate(&stream_tags),
                320_000.0
            );
        }

        #[test]
        fn a_zero_header_bitrate_falls_back_to_the_bps_tag() {
            let stream_tags = tags(&[("BPS", "8000000")]);
            assert_eq!(video_metadata(0).actual_bit_rate(&stream_tags), 8_000_000.0);
            assert_eq!(
                audio_metadata(0, 0, 2).actual_bit_rate(&stream_tags),
                8_000_000.0
            );
        }

        #[test]
        fn a_malformed_bps_tag_yields_zero_rather_than_a_panic() {
            let stream_tags = tags(&[("BPS", "not-a-number")]);
            assert_eq!(video_metadata(0).actual_bit_rate(&stream_tags), 0.0);
            assert_eq!(audio_metadata(0, 0, 2).actual_bit_rate(&stream_tags), 0.0);
        }

        #[test]
        fn no_bitrate_anywhere_is_zero() {
            let none = HashMap::new();
            assert_eq!(video_metadata(0).actual_bit_rate(&none), 0.0);
            assert_eq!(audio_metadata(0, 0, 2).actual_bit_rate(&none), 0.0);
        }
    }

    mod frame_count_fallback {
        use super::*;

        #[test]
        fn the_header_frame_count_wins_when_it_is_present() {
            assert_eq!(
                video_stream(0, 1234, tags(&[("NUMBER_OF_FRAMES", "9")])).actual_frames(),
                1234
            );
            assert_eq!(
                audio_metadata(0, 1234, 2).actual_frames(&tags(&[("NUMBER_OF_FRAMES", "9")])),
                1234
            );
        }

        #[test]
        fn a_zero_frame_count_falls_back_to_the_tag() {
            assert_eq!(
                video_stream(0, 0, tags(&[("NUMBER_OF_FRAMES", "43200")])).actual_frames(),
                43_200
            );
            assert_eq!(
                audio_metadata(0, 0, 2).actual_frames(&tags(&[("NUMBER_OF_FRAMES", "43200")])),
                43_200
            );
        }

        #[test]
        fn a_malformed_or_missing_tag_yields_zero() {
            assert_eq!(
                video_stream(0, 0, tags(&[("NUMBER_OF_FRAMES", "lots")])).actual_frames(),
                0
            );
            assert_eq!(video_stream(0, 0, HashMap::new()).actual_frames(), 0);
            assert_eq!(audio_metadata(0, 0, 2).actual_frames(&HashMap::new()), 0);
        }
    }

    mod duration_fallback {
        use super::*;

        const FILE_DURATION: f64 = 7200.0;

        #[test]
        fn the_stream_duration_wins_when_it_is_present() {
            // 60_000 ticks at 1/1000 = 60 seconds.
            let stream = video_stream(60_000, 0, tags(&[("DURATION", "01:00:00.0")]));
            assert_eq!(stream.duration_seconds(), 60.0);
            assert_eq!(stream.actual_duration_seconds(FILE_DURATION), 60.0);
        }

        #[test]
        fn a_zero_stream_duration_falls_back_to_the_duration_tag() {
            let stream = video_stream(0, 0, tags(&[("DURATION", "00:02:30.0")]));
            assert_eq!(stream.actual_duration_seconds(FILE_DURATION), 150.0);

            let subtitles = subtitle_stream(0, tags(&[("DURATION", "00:02:30.0")]));
            assert_eq!(subtitles.actual_duration_seconds(FILE_DURATION), 150.0);
        }

        #[test]
        fn an_unparseable_duration_tag_falls_through_to_the_file_duration() {
            // Better a plausible container-level duration than a zero-length
            // track the player refuses to seek in.
            let stream = video_stream(0, 0, tags(&[("DURATION", "garbage")]));
            assert_eq!(stream.actual_duration_seconds(FILE_DURATION), FILE_DURATION);

            let subtitles = subtitle_stream(0, tags(&[("DURATION", "garbage")]));
            assert_eq!(
                subtitles.actual_duration_seconds(FILE_DURATION),
                FILE_DURATION
            );
        }

        #[test]
        fn no_duration_anywhere_falls_through_to_the_file_duration() {
            assert_eq!(
                video_stream(0, 0, HashMap::new()).actual_duration_seconds(FILE_DURATION),
                FILE_DURATION
            );
            assert_eq!(
                subtitle_stream(0, HashMap::new()).actual_duration_seconds(FILE_DURATION),
                FILE_DURATION
            );
        }
    }

    mod derived_descriptions {
        use super::*;

        #[test]
        fn channel_counts_map_to_the_layout_names_users_recognise() {
            assert_eq!(audio_metadata(0, 0, 1).channel_layout_description(), "Mono");
            assert_eq!(
                audio_metadata(0, 0, 2).channel_layout_description(),
                "Stereo"
            );
            assert_eq!(audio_metadata(0, 0, 6).channel_layout_description(), "5.1");
            assert_eq!(audio_metadata(0, 0, 8).channel_layout_description(), "7.1");
            // Anything else is described generically rather than guessed at.
            for channels in [0, 3, 4, 5, 7, 16] {
                assert_eq!(
                    audio_metadata(0, 0, channels).channel_layout_description(),
                    "Multi-channel",
                    "for {channels} channels"
                );
            }
        }

        #[test]
        fn a_streams_unique_id_distinguishes_codec_and_resolution() {
            let hd = video_stream(0, 0, HashMap::new());
            assert_eq!(hd.unique_id(), "h264-1920x1080");

            let mut sd = video_stream(0, 0, HashMap::new());
            sd.video.width = 640;
            sd.video.height = 480;
            assert_ne!(hd.unique_id(), sd.unique_id());

            let mut other_codec = video_stream(0, 0, HashMap::new());
            other_codec.video.codec_name = "hevc".to_string();
            assert_ne!(hd.unique_id(), other_codec.unique_id());
        }

        #[test]
        fn frame_rate_is_the_streams_rational_rate() {
            // 24000/1001 is NTSC film: a rational the probe must not round to
            // 24, or A/V sync drifts a frame every ~17 minutes.
            let stream = video_stream(0, 0, HashMap::new());
            let rate = stream.frame_rate().expect("a rate was probed");
            assert!((rate - 23.976_023_976).abs() < 1e-6, "got {rate}");
        }

        #[test]
        fn a_stream_without_a_rate_reports_none_rather_than_zero() {
            let mut stream = video_stream(0, 0, HashMap::new());
            stream.rate = None;
            assert_eq!(stream.frame_rate(), None);
        }

        #[test]
        fn video_bit_depth_and_resolution_come_from_the_probed_format() {
            let video = video_metadata(0);
            assert_eq!(video.resolution(), Resolution::new(1920, 1080));
            assert_eq!(video.bit_depth(), PixelFormat::YUV420P.bit_depth());
        }
    }

    mod subtitle_tags {
        use super::*;

        #[test]
        fn title_and_language_are_read_from_the_stream_tags() {
            let subtitles = subtitle_stream(
                1000,
                tags(&[("title", "Forced (Alien)"), ("language", "eng")]),
            );
            assert_eq!(subtitles.title().as_deref(), Some("Forced (Alien)"));
            assert_eq!(subtitles.language().as_deref(), Some("eng"));
        }

        #[test]
        fn an_untagged_subtitle_track_reports_none_rather_than_an_empty_string() {
            // A `Some("")` would render as a blank entry in the track picker.
            let subtitles = subtitle_stream(1000, HashMap::new());
            assert_eq!(subtitles.title(), None);
            assert_eq!(subtitles.language(), None);
        }
    }
}
