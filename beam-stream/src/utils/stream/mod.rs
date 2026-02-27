use num::{
    Rational32, ToPrimitive,
    integer::{gcd, lcm},
};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::trace;

use crate::utils::{
    codec::{OutputAudioCodec, OutputSubtitleCodec, OutputVideoCodec},
    file::FileType,
    hash::XXH3Hash,
    metadata::{MetadataError, StreamMetadata},
    stream::config::{AudioStream, OutputStream, SubtitleStream, VideoStream},
};
use config::StreamConfiguration;

pub mod config;
pub mod hls;
pub mod mp4;

use std::sync::Arc;

use crate::services::hash::HashService;
use crate::services::media_info::MediaInfoService;

#[derive(Debug, Clone)]
pub struct StreamBuilder {
    /// List of files to process into HLS stream
    files: Vec<(FileType, PathBuf)>,
    hash_service: Arc<dyn HashService>,
    media_info_service: Arc<dyn MediaInfoService>,
}

impl StreamBuilder {
    pub fn new(
        hash_service: Arc<dyn HashService>,
        media_info_service: Arc<dyn MediaInfoService>,
    ) -> Self {
        Self {
            files: Vec::new(),
            hash_service,
            media_info_service,
        }
    }

    /// Add video file to merge into HLS stream
    pub fn add_file(&mut self, file_type: FileType, file_path: &Path) -> &mut Self {
        self.files.push((file_type, file_path.to_path_buf()));
        self
    }

    /// Build the stream configuration by processing all added files.
    pub async fn build(self) -> Result<StreamConfiguration, StreamBuilderError> {
        // Ensure at least one video file is provided
        if !self.files.iter().any(|(ft, _)| *ft == FileType::Video) {
            return Err(StreamBuilderError::NoVideoFiles);
        }

        // ===== Pre-process streams =====
        // Detect GOP sizes (in seconds) for each video stream
        let gop_sizes: Vec<Rational32> = vec![Rational32::new(48, 24)]; // TODO: Detect GOP sizes from video streams
        // We need to find the lowest common multiple of all GOP sizes to ensure all video streams can be aligned
        let lcm_size: Rational32 = gop_sizes.iter().fold(Rational32::new(1, 1), |acc, &gop| {
            Rational32::new(
                lcm(*acc.numer(), *gop.numer()),
                gcd(*acc.denom(), *gop.denom()),
            )
            // let gcd = acc.lcm(&gop);
            // (acc * gop) / gcd
        });

        // Determine target segment duration (in seconds) based on GOP sizes
        // 6 second is a good default but we want to make sure `lcm_size` strictly divides target duration
        // i.e., target_duration = k * lcm_size where k = ⌈6*b/a⌉
        let target_duration: u64 = {
            let six = Rational32::new(6, 1);
            let k = (six * lcm_size.recip()).ceil();
            (k * lcm_size).ceil().to_u64().unwrap()
        };

        let mut sources: Vec<_> = vec![];
        let mut streams: Vec<OutputStream> = vec![];

        // Generate stream configuration
        for (i, (file_type, file_path)) in self.files.into_iter().enumerate() {
            trace!("Processing file: {:?}", &file_path);
            // Ensure path exists
            if !file_path.exists() || !file_path.is_file() {
                return Err(StreamBuilderError::FileNotFound(file_path.clone()));
            }

            // Open file and acquire shared lock to prevent modifications during processing
            let file = std::fs::File::open(&file_path)
                .map_err(|e| StreamBuilderError::FileLockError(file_path.clone(), e))?;
            fs2::FileExt::lock_shared(&file)
                .map_err(|e| StreamBuilderError::FileLockError(file_path.clone(), e))?;

            // Spawn concurrent tasks: metadata extraction and file hashing
            // Both will hold the shared lock until they complete
            let hash_path = file_path.clone();
            let hash_service = self.hash_service.clone();
            let media_info_service = self.media_info_service.clone();

            let hash_task = tokio::spawn(async move {
                trace!("Hashing file: {:?}", &hash_path);
                hash_service.hash_async(hash_path).await
            });

            // Process metadata extraction in current task
            let metadata_result = async {
                match file_type {
                    FileType::Video => {
                        trace!("Extracting video metadata: {:?}", &file_path);
                        // Process video file
                        let file_metadata =
                            media_info_service.get_video_metadata(&file_path).await?;
                        trace!(
                            "Extracted video metadata for file {:?}: {:#?}",
                            &file_path, &file_metadata
                        );
                        let best_audio_stream_idx = file_metadata.best_audio_stream;
                        let best_subtitle_stream_idx = file_metadata.best_subtitle_stream;

                        let mut local_streams = Vec::new();

                        for (j, stream_metadata) in file_metadata.streams.iter().enumerate() {
                            match stream_metadata {
                                StreamMetadata::Video(stream_metadata) => {
                                    // Append video stream
                                    let stream = VideoStream {
                                        source_file_index: i,
                                        source_stream_index: j,
                                        codec: OutputVideoCodec::Remuxed(
                                            stream_metadata.video.codec_name.clone(),
                                        ),
                                        max_rate: stream_metadata.video.max_rate,
                                        bit_rate: stream_metadata.video.bit_rate,
                                        resolution: stream_metadata.video.resolution(),
                                        frame_rate: stream_metadata
                                            .rate
                                            .unwrap_or(Rational32::new(0, 1)), // TODO: handle this properly
                                    };

                                    local_streams.push(OutputStream::Video(stream));

                                    // TODO: Add audio stream if present vv. Need metadata.rs to extract audio metadata out of video track as well? does ffmpeg always separate the video and audio streams?
                                    // // Append corresponding audio stream (required by CMAF)
                                    // let stream = AudioStream {
                                    //     source_file_index: i,
                                    //     source_stream_index: j,
                                    //     codec: OutputAudioCodec::Remuxed(
                                    //         stream_metadata.video.codec_name.clone(),
                                    //     ),
                                    //     // max_rate: stream_metadata.video.max_rate,
                                    //     // bit_rate: stream_metadata.video.bit_rate,
                                    //     // resolution: stream_metadata.video.resolution(),
                                    //     // frame_rate: stream_metadata.rate,
                                    // };

                                    // local_streams.push(OutputStream::Audio(stream));
                                }
                                StreamMetadata::Audio(stream_metadata) => {
                                    let stream = AudioStream {
                                        source_file_index: i,
                                        source_stream_index: j,
                                        codec: OutputAudioCodec::Remuxed(
                                            // TODO: Verify we don't transcode audio unnecessarily
                                            stream_metadata.audio.codec_name.clone(),
                                        ),
                                        language: {
                                            let s = &stream_metadata.audio.language;
                                            if s.is_empty() { None } else { Some(s.clone()) }
                                        },
                                        title: stream_metadata.audio.title.clone(),
                                        channel_layout: stream_metadata
                                            .audio
                                            .channel_layout
                                            .description(),
                                        is_default: Some(j) == best_audio_stream_idx, // TODO: Verify this works
                                        is_autoselect: true, // TODO: Verify this is correct
                                    };

                                    local_streams.push(OutputStream::Audio(stream));
                                }
                                StreamMetadata::Subtitle(stream_metadata) => {
                                    let stream = SubtitleStream {
                                        source_file_index: i,
                                        source_stream_index: j,
                                        codec: OutputSubtitleCodec::WebVTT, // We just use WebVTT no matter what for now (CMAF-compliant)
                                        language: stream_metadata.language(),
                                        title: stream_metadata.title(),
                                        is_default: Some(j) == best_subtitle_stream_idx, // TODO: Verify this works
                                        is_autoselect: true,
                                        is_forced: false, // TODO: Detect forced subtitles
                                    };

                                    local_streams.push(OutputStream::Subtitle(stream));
                                }
                            }
                        }

                        Ok::<Vec<OutputStream>, StreamBuilderError>(local_streams)
                    }
                    FileType::Subtitle => {
                        trace!("Processing subtitle file: {:?}", &file_path);
                        // Process subtitle file
                        // TODO: Finish this
                        Ok(Vec::new())
                    }
                }
            }
            .await?;
            trace!(
                "Extracted metadata for file {:?}: {:#?}",
                &file_path, &metadata_result
            );

            // Wait for hash computation to complete
            let hash_value = hash_task
                .await
                .map_err(StreamBuilderError::HashTaskError)?
                .map_err(|e| StreamBuilderError::HashError(file_path.clone(), e))?;
            trace!("Computed hash for file {:?}: {}", &file_path, &hash_value);

            // Release the shared lock now that both tasks are complete
            fs2::FileExt::unlock(&file)
                .map_err(|e| StreamBuilderError::FileLockError(file_path.clone(), e))?;
            drop(file);

            // Append processed streams
            streams.extend(metadata_result);

            // Append to sources
            sources.push((file_type, file_path, XXH3Hash::new(hash_value)))
        }

        Ok(StreamConfiguration {
            sources,
            streams,
            target_duration,
        })
    }
}

#[derive(Debug, Error)]
pub enum StreamBuilderError {
    #[error("No video files provided")]
    NoVideoFiles,
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),
    #[error("Failed to read video metadata: {0}")]
    VideoMetadataError(#[from] MetadataError),
    #[error("Failed to acquire file lock on {0}: {1}")]
    FileLockError(PathBuf, std::io::Error),
    #[error("Hash task failed: {0}")]
    HashTaskError(tokio::task::JoinError),
    #[error("Failed to hash file {0}: {1}")]
    HashError(PathBuf, std::io::Error),
}
