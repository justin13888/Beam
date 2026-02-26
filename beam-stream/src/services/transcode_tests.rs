#[cfg(test)]
mod tests {
    use crate::services::transcode::{LocalTranscodeService, TranscodeService};
    use crate::utils::color::{
        ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
        PixelFormat,
    };
    use crate::utils::metadata::{
        Rational, StreamMetadata, VideoFileMetadata, VideoMetadata, VideoStreamMetadata,
    };
    use beam_index::services::hash::MockHashService;
    use beam_index::services::media_info::MockMediaInfoService;
    use std::collections::HashMap;
    // use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_dummy_video(path: &std::path::Path) {
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=30",
                "-c:v",
                "libx264",
                "-t",
                "1",
                path.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to execute ffmpeg");

        if !status.status.success() {
            panic!("ffmpeg failed: {}", String::from_utf8_lossy(&status.stderr));
        }
    }

    #[tokio::test]
    async fn test_generate_mp4_cache_success() {
        // Setup temp dir
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.mp4");
        let output_path = temp_dir.path().join("output.mp4");

        // Create dummy video
        create_dummy_video(&source_path);

        // Mocks
        let mut mock_hash_service = MockHashService::new();
        let mut mock_media_info_service = MockMediaInfoService::new();

        // Hash Service Expectation
        mock_hash_service
            .expect_hash_async()
            .returning(|_| Ok(12345));

        // Media Info Expectation
        let metadata_source_path = source_path.clone();
        mock_media_info_service
            .expect_get_video_metadata()
            .returning(move |_| {
                Ok(VideoFileMetadata {
                    file_path: metadata_source_path.clone(),
                    metadata: HashMap::default(),
                    best_video_stream: Some(0),
                    best_audio_stream: None,
                    best_subtitle_stream: None,
                    duration: 1000000, // 1s
                    streams: vec![StreamMetadata::Video(VideoStreamMetadata {
                        index: 0,
                        time_base: Rational::new(1, 30),
                        start_time: 0,
                        duration: 30, // 1 sec at 30fps timebase? Wait, timebase is likely 1/30.
                        frames: 30,
                        disposition: crate::utils::format::Disposition::default(),
                        discard: crate::utils::media::Discard::None,
                        rate: Some(Rational::new(30, 1)),
                        codec_id: crate::utils::media::CodecId::H264,
                        metadata: HashMap::default(),
                        video: VideoMetadata {
                            bit_rate: 1000,
                            max_rate: 1000,
                            delay: 0,
                            width: 320,
                            height: 240,
                            format: PixelFormat::None, // Mocking
                            has_b_frames: false,
                            aspect_ratio: Rational::new(1, 1),
                            color_space: ColorSpace::BT709,
                            color_range: ColorRange::MPEG,
                            color_primaries: ColorPrimaries::BT709,
                            color_transfer_characteristic: ColorTransferCharacteristic::BT709,
                            chroma_location: ChromaLocation::Left,
                            references: 0,
                            intra_dc_precision: 0,
                            profile: "High".to_string(),
                            level: "30".to_string(),
                            codec_name: "h264".to_string(),
                        },
                    })],
                    format_name: "mp4".to_string(),
                    format_long_name: "MPEG-4".to_string(),
                    file_size: 1024,
                    bit_rate: 1000,
                    probe_score: 100,
                })
            });

        // Service
        let service = LocalTranscodeService::new(
            Arc::new(mock_hash_service),
            Arc::new(mock_media_info_service),
        );

        // Execute
        let result = service.generate_mp4_cache(&source_path, &output_path).await;

        // Verify
        if let Err(e) = &result {
            println!("Transcode failed error: {:?}", e);
        }
        assert!(result.is_ok(), "Transcode failed: {:?}", result.err());
        assert!(output_path.exists(), "Output file not created");
    }
}
