use ffmpeg_next as ffmpeg;
use num::rational::Ratio;
use num::traits::cast::ToPrimitive;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tracing::trace;

use crate::utils::{
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
    pub bit_rate: usize,
    pub max_rate: usize,
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
    pub bit_rate: usize,
    pub max_rate: usize,
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
                        bit_rate: video_decoder.bit_rate(),
                        max_rate: video_decoder.max_bit_rate(),
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
                        bit_rate: audio_decoder.bit_rate(),
                        max_rate: audio_decoder.max_bit_rate(),
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
        let format_long_name = context.format().description().to_string();
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
