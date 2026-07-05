use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::config::{OutputStream, StreamConfiguration};

pub const MP4_VIDEO_PATH: &str = "index.mp4";
pub const MP4_METADATA_PATH: &str = "index.json";

pub struct MP4StreamGenerator {
    configuration: StreamConfiguration,
}

impl MP4StreamGenerator {
    pub fn new(configuration: StreamConfiguration) -> Self {
        Self { configuration }
    }

    /// Get metadata for the fMP4 stream
    pub fn get_metadata(&self) -> MP4StreamMetadata {
        MP4StreamMetadata {
            subtitle_tracks: self
                .configuration
                .subtitle_streams()
                .iter()
                .map(|ss| MP4SubtitleTrack {
                    language: ss.language.clone(),
                    title: ss.title.clone(),
                    is_default: ss.is_default,
                    is_autoselect: ss.is_autoselect,
                    is_forced: ss.is_forced,
                })
                .collect(),
        }
    }

    /// Generate fMP4 file to file path
    pub async fn generate_mp4(&self, output_path: &Path) -> Result<(), MP4StreamGeneratorError> {
        // Spawn a blocking task for the FFmpeg processing
        let config = self.configuration.clone();
        let output_path = output_path.to_path_buf();

        tokio::task::spawn_blocking(move || Self::generate_mp4_blocking(&config, &output_path))
            .await
            .map_err(|e| {
                MP4StreamGeneratorError::IOError(std::io::Error::other(format!(
                    "Task join error: {}",
                    e
                )))
            })??;

        Ok(())
    }

    /// Blocking implementation of MP4 generation
    fn generate_mp4_blocking(
        config: &StreamConfiguration,
        output_path: &Path,
    ) -> Result<(), MP4StreamGeneratorError> {
        use ffmpeg_next as ffmpeg;

        // Open all input files
        let mut inputs: Vec<ffmpeg::format::context::Input> = config
            .sources
            .iter()
            .map(|(_, path, _)| ffmpeg::format::input(&path))
            .collect::<Result<Vec<_>, ffmpeg_next::Error>>()?;

        // Create output context with explicit mp4 format
        let mut output = ffmpeg::format::output_as(&output_path, "mp4")?;

        // Map streams from inputs to output
        let mut stream_mapping: Vec<(usize, usize, usize)> = Vec::new(); // (input_idx, input_stream_idx, output_stream_idx)

        // Keep temporary VTT files alive until we're done processing
        let mut _temp_vtt_files: Vec<tempfile::NamedTempFile> = Vec::new();

        for stream_config in &config.streams {
            match stream_config {
                OutputStream::Video(vs) => {
                    let input = &inputs[vs.source_file_index];
                    let input_stream = input
                        .stream(vs.source_stream_index)
                        .ok_or(MP4StreamGeneratorError::StreamNotFound)?;

                    let mut output_stream =
                        output.add_stream(ffmpeg::encoder::find(input_stream.parameters().id()))?;
                    output_stream.set_parameters(input_stream.parameters());

                    // Don't copy time_base - let ffmpeg handle it based on the container format
                    // Copying time_base can cause FPS calculation issues

                    stream_mapping.push((
                        vs.source_file_index,
                        vs.source_stream_index,
                        output_stream.index(),
                    ));
                }
                OutputStream::Audio(as_) => {
                    let input = &inputs[as_.source_file_index];
                    let input_stream = input
                        .stream(as_.source_stream_index)
                        .ok_or(MP4StreamGeneratorError::StreamNotFound)?;

                    let mut output_stream =
                        output.add_stream(ffmpeg::encoder::find(input_stream.parameters().id()))?;
                    output_stream.set_parameters(input_stream.parameters());

                    // Set language metadata if available
                    if let Some(lang) = &as_.language {
                        output_stream.set_metadata(ffmpeg::Dictionary::from_iter(vec![(
                            "language",
                            lang.as_str(),
                        )]));
                    }

                    stream_mapping.push((
                        as_.source_file_index,
                        as_.source_stream_index,
                        output_stream.index(),
                    ));
                }
                OutputStream::Subtitle(ss) => {
                    let input = &inputs[ss.source_file_index];
                    let input_stream = input
                        .stream(ss.source_stream_index)
                        .ok_or(MP4StreamGeneratorError::StreamNotFound)?;

                    let codec_id = input_stream.parameters().id();

                    // Case 1: Subtitle codec is already supported in MP4 (MOV_TEXT or WEBVTT)
                    if is_subtitle_codec_supported_in_mp4(codec_id) {
                        let mut output_stream =
                            output.add_stream(ffmpeg::encoder::find(codec_id))?;
                        output_stream.set_parameters(input_stream.parameters());

                        // Set subtitle metadata
                        let mut metadata_pairs = Vec::new();
                        if let Some(lang) = &ss.language {
                            metadata_pairs.push(("language", lang.as_str()));
                        }
                        if let Some(title) = &ss.title {
                            metadata_pairs.push(("title", title.as_str()));
                        }
                        if !metadata_pairs.is_empty() {
                            output_stream
                                .set_metadata(ffmpeg::Dictionary::from_iter(metadata_pairs));
                        }

                        stream_mapping.push((
                            ss.source_file_index,
                            ss.source_stream_index,
                            output_stream.index(),
                        ));
                    } else {
                        // Case 2: Subtitle codec is not supported - convert to MOV_TEXT
                        // This requires a two-pass approach:
                        // 1. Extract subtitle to temporary .mp4 file with MOV_TEXT codec
                        // 2. Re-open the .mp4 file and add it as a MOV_TEXT stream

                        // Create a temporary file for the converted subtitle
                        let temp_sub_file = tempfile::Builder::new()
                            .suffix(".mp4")
                            .tempfile()
                            .map_err(|e| {
                                MP4StreamGeneratorError::IOError(std::io::Error::other(format!(
                                    "Failed to create temporary subtitle file: {}",
                                    e
                                )))
                            })?;
                        let temp_sub_path = temp_sub_file.path();

                        // Extract and convert subtitle to MOV_TEXT using FFmpeg
                        Self::extract_subtitle_to_mov_text(
                            &config.sources[ss.source_file_index].1,
                            ss.source_stream_index,
                            temp_sub_path,
                        )?;

                        // Open the converted subtitle file as a new input
                        let sub_input = ffmpeg::format::input(&temp_sub_path)?;

                        // Find the subtitle stream in the file (should be stream 0)
                        let sub_stream = sub_input
                            .stream(0)
                            .ok_or(MP4StreamGeneratorError::StreamNotFound)?;

                        // Add MOV_TEXT stream to output
                        let mov_text_codec_id = ffmpeg_next::codec::Id::MOV_TEXT;
                        let encoder =
                            ffmpeg::encoder::find(mov_text_codec_id).ok_or_else(|| {
                                MP4StreamGeneratorError::IOError(std::io::Error::other(
                                    "MOV_TEXT encoder not found",
                                ))
                            })?;
                        let mut output_stream = output.add_stream(encoder)?;
                        output_stream.set_parameters(sub_stream.parameters());

                        // Set subtitle metadata
                        let mut metadata_pairs = Vec::new();
                        if let Some(lang) = &ss.language {
                            metadata_pairs.push(("language", lang.as_str()));
                        }
                        if let Some(title) = &ss.title {
                            metadata_pairs.push(("title", title.as_str()));
                        }
                        if !metadata_pairs.is_empty() {
                            output_stream
                                .set_metadata(ffmpeg::Dictionary::from_iter(metadata_pairs));
                        }

                        // Store the subtitle input and add to stream mapping
                        let sub_input_idx = inputs.len();
                        inputs.push(sub_input);

                        stream_mapping.push((
                            sub_input_idx,
                            0, // Subtitle file only has one stream at index 0
                            output_stream.index(),
                        ));

                        // Keep the temporary file alive until processing is done
                        _temp_vtt_files.push(temp_sub_file);
                    }
                }
            }
        }

        // Set movflags on the output format context
        output.set_metadata(ffmpeg::Dictionary::from_iter(vec![(
            "movflags",
            "frag_keyframe+empty_moov+default_base_moof",
        )]));

        // Write header
        output.write_header()?;

        // Process packets from all input streams properly
        // Pre-compute time_base mappings to avoid borrowing issues
        let mut time_base_mappings = Vec::new();
        for (input_idx, input) in inputs.iter().enumerate() {
            for &(in_idx, in_stream_idx, output_stream_idx) in &stream_mapping {
                if in_idx == input_idx {
                    let input_tb = input.stream(in_stream_idx).unwrap().time_base();
                    let output_tb = output.stream(output_stream_idx).unwrap().time_base();
                    time_base_mappings.push((
                        input_idx,
                        in_stream_idx,
                        output_stream_idx,
                        input_tb,
                        output_tb,
                    ));
                }
            }
        }

        // Read packets from each input until exhausted
        for (input_idx, input) in inputs.iter_mut().enumerate() {
            for (stream, packet) in input.packets() {
                // Find corresponding output stream and time_base mapping
                if let Some(&(_, _, output_stream_idx, input_tb, output_tb)) = time_base_mappings
                    .iter()
                    .find(|(in_idx, in_stream_idx, _, _, _)| {
                        *in_idx == input_idx && *in_stream_idx == stream.index()
                    })
                {
                    let mut packet = packet;

                    // Rescale timestamps from input time_base to output time_base
                    // This must be done unconditionally to ensure timestamps are properly set
                    packet.rescale_ts(input_tb, output_tb);

                    packet.set_stream(output_stream_idx);
                    packet.write_interleaved(&mut output)?;
                }
            }
        }

        // Write trailer
        output.write_trailer()?;

        Ok(())
    }

    /// Extract a subtitle stream to MOV_TEXT format in an MP4 container
    /// This is used for converting unsupported subtitle formats (e.g., SRT, ASS, SSA) to MOV_TEXT
    ///
    /// Equivalent to: ffmpeg -i input -map 0:stream_index -c:s mov_text output.mp4
    ///
    /// Note: ffmpeg-next doesn't expose subtitle encode/decode APIs, so we use
    /// std::process::Command as a fallback. This is actually the recommended approach
    /// for subtitle transcoding as it's more stable than using the low-level FFI directly.
    fn extract_subtitle_to_mov_text(
        input_path: &Path,
        stream_index: usize,
        output_mp4_path: &Path,
    ) -> Result<(), MP4StreamGeneratorError> {
        use std::process::Command;

        let output = Command::new("ffmpeg")
            .arg("-y") // Overwrite output file without asking
            .arg("-i")
            .arg(input_path)
            .arg("-map")
            .arg(format!("0:{}", stream_index)) // Map specific subtitle stream
            .arg("-c:s")
            .arg("mov_text") // Convert to MOV_TEXT codec (MP4 subtitle format)
            .arg(output_mp4_path)
            .output()
            .map_err(|e| {
                MP4StreamGeneratorError::IOError(std::io::Error::other(format!(
                    "Failed to execute ffmpeg command: {}. Is ffmpeg installed?",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(MP4StreamGeneratorError::IOError(std::io::Error::other(
                format!(
                    "FFmpeg subtitle extraction failed with exit code {:?}\nStderr: {}\nStdout: {}",
                    output.status.code(),
                    stderr,
                    stdout
                ),
            )));
        }

        Ok(())
    }
}

/// Check if a subtitle codec is supported in MP4 containers
fn is_subtitle_codec_supported_in_mp4(codec_id: ffmpeg_next::codec::Id) -> bool {
    use ffmpeg_next::codec::Id;

    // MP4 primarily supports MOV_TEXT (also known as TX3G) for subtitles
    // While WEBVTT is technically supported in MP4 (as "wvtt"), MOV_TEXT has better compatibility
    // across players and browsers, so we only accept MOV_TEXT as natively supported
    // Unsupported: SUBRIP, ASS, SSA, DVB_SUBTITLE, WEBVTT, etc. will be converted to MOV_TEXT
    matches!(codec_id, Id::MOV_TEXT)
}

impl From<StreamConfiguration> for MP4StreamGenerator {
    fn from(configuration: StreamConfiguration) -> Self {
        Self::new(configuration)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MP4StreamMetadata {
    pub subtitle_tracks: Vec<MP4SubtitleTrack>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MP4SubtitleTrack {
    /// The ISO 639-2/B 3-letter language code (e.g., "eng", "spa").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// A descriptive title for the subtitle track (e.g., "SDH", "Commentary").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Default means client should select this track if no other preference is given.
    pub is_default: bool,

    /// Autoselect means client may automatically choose, typically based on user preferences (e.g. system language).
    pub is_autoselect: bool,

    /// Flag indicating if this is a "forced" subtitle track (e.g., for foreign audio only).
    pub is_forced: bool,
}

#[derive(Debug, Error)]
pub enum MP4StreamGeneratorError {
    #[error("FFmpeg error: {0}")]
    FFmpegError(#[from] ffmpeg_next::Error),

    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("Stream not found in input file")]
    StreamNotFound,
}
