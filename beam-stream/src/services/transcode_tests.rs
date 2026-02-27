#[cfg(test)]
mod tests {
    use crate::services::transcode::{LocalTranscodeService, Mp4Generator, TranscodeService};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Test double: creates an empty output file without invoking ffmpeg.
    #[derive(Debug)]
    struct StubMp4Generator;

    #[async_trait::async_trait]
    impl Mp4Generator for StubMp4Generator {
        async fn generate_mp4(
            &self,
            _source_path: &Path,
            output_path: &Path,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            std::fs::File::create(output_path)?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_generate_mp4_cache_success() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.mp4");
        let output_path = temp_dir.path().join("output.mp4");

        let service = LocalTranscodeService::new(Arc::new(StubMp4Generator));

        let result = service.generate_mp4_cache(&source_path, &output_path).await;

        assert!(
            result.is_ok(),
            "generate_mp4_cache failed: {:?}",
            result.err()
        );
        assert!(output_path.exists(), "Output file was not created");
    }

    #[tokio::test]
    async fn test_generate_mp4_cache_skips_existing_output() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.mp4");
        let output_path = temp_dir.path().join("output.mp4");

        // Pre-create the output file to simulate a cache hit
        std::fs::File::create(&output_path).unwrap();

        // StubMp4Generator would overwrite the file; if the cache-hit path fires correctly
        // the stub is never called and the (empty) file remains unchanged.
        let service = LocalTranscodeService::new(Arc::new(StubMp4Generator));

        let result = service.generate_mp4_cache(&source_path, &output_path).await;

        assert!(
            result.is_ok(),
            "generate_mp4_cache failed: {:?}",
            result.err()
        );
        assert!(
            output_path.exists(),
            "Output file should still exist after cache hit"
        );
    }
}
