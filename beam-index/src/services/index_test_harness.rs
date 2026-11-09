//! The helpers the index service's tests share: a library, the
//! services they are built from, and probed metadata to feed them.

use super::*;
use crate::probe::color::{
    ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
    PixelFormat,
};
use crate::probe::format::{ChannelLayout, Disposition, SampleFormat};
use crate::probe::media::{CodecId, Discard};
use crate::probe::metadata::StreamMetadata as UtilStreamMetadata;
use crate::probe::metadata::{
    AudioMetadata, AudioStreamMetadata as UtilAudioStream,
    SubtitleStreamMetadata as UtilSubtitleStream, VideoFileMetadata, VideoMetadata,
    VideoStreamMetadata as UtilVideoStream,
};
use crate::services::admin_log::NoOpAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::{CreateLibrary, Library};
use beam_domain::repositories::file::MockFileRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::MockLibraryRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::MockMovieRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::MockShowRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use num::rational::Ratio;
use std::path::PathBuf;
use tempfile::TempDir;

// ─── helpers ─────────────────────────────────────────────────────────────

/// `beam-server` passes an `IndexError::PathNotFound` message straight
/// through as the `detail` of a 400 an administrator's browser renders, so
/// the guarantee written on the variant has to hold at *every* construction
/// site (NFR-108). The path reaches the operator through the `tracing`
/// field, the admin notification and the admin log instead.
pub(super) fn assert_names_no_path(err: &IndexError, paths: &[&Path]) {
    let message = err.to_string();
    for path in paths {
        let path = path.to_string_lossy();
        assert!(
            !message.contains(path.as_ref()),
            "a client-facing rejection must not carry a filesystem path; \
             {message:?} names {path:?}"
        );
    }
    assert!(
        !message.contains(std::path::MAIN_SEPARATOR),
        "a client-facing rejection must not carry any path component: {message:?}"
    );
}

/// A library rooted at `root`, for calling a per-file method directly.
pub(super) fn test_library(id: Uuid, root: &Path) -> Library {
    Library {
        id,
        name: "Test".to_string(),
        root_path: root.to_path_buf(),
        description: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        last_scan_started_at: None,
        last_scan_finished_at: None,
        last_scan_file_count: None,
    }
}

impl LocalIndexService {
    /// Classify `path` in a library rooted at its first component
    /// (`/media` for `/media/Show/x.mkv`; nothing for a bare filename),
    /// expecting a classification.
    pub(super) async fn classify_for_test(
        &self,
        path: &Path,
        lib_id: Uuid,
        runtime: Duration,
    ) -> Result<MediaFileContent, IndexError> {
        let root: PathBuf = path.components().take(2).collect();
        let root = if path.is_absolute() {
            root
        } else {
            PathBuf::new()
        };
        let content = self
            .classify_media_content(
                path,
                &test_library(lib_id, &root),
                Some(runtime),
                &ContainerTags::default(),
            )
            .await?;
        Ok(content.expect("the path classifies"))
    }
}

pub(super) fn make_classify_service() -> (
    LocalIndexService,
    Arc<InMemoryMovieRepository>,
    Arc<InMemoryShowRepository>,
) {
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let show_repo = Arc::new(InMemoryShowRepository::default());
    let service = LocalIndexService::new(
        Arc::new(InMemoryLibraryRepository::default()),
        Arc::new(InMemoryFileRepository::default()),
        movie_repo.clone(),
        show_repo.clone(),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(NoOpAdminLogService),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );
    (service, movie_repo, show_repo)
}

pub(super) fn make_service_with_stream_repo(
    stream_repo: Arc<InMemoryMediaStreamRepository>,
) -> LocalIndexService {
    LocalIndexService::new(
        Arc::new(MockLibraryRepository::new()),
        Arc::new(MockFileRepository::new()),
        Arc::new(MockMovieRepository::new()),
        Arc::new(MockShowRepository::new()),
        stream_repo,
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(NoOpAdminLogService),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    )
}

pub(super) fn make_video_stream(
    index: usize,
    width: u32,
    height: u32,
    bit_rate: u64,
    codec_name: &str,
    frame_rate: Option<Ratio<i32>>,
) -> UtilStreamMetadata {
    UtilStreamMetadata::Video(UtilVideoStream {
        index,
        time_base: Ratio::new(1, 1000),
        start_time: 0,
        duration: 1_000_000,
        frames: 0,
        disposition: Disposition::default(),
        discard: Discard::Default,
        rate: frame_rate,
        codec_id: CodecId::H264,
        video: VideoMetadata {
            bit_rate,
            max_rate: 0,
            delay: 0,
            width,
            height,
            format: PixelFormat::None,
            has_b_frames: false,
            aspect_ratio: Ratio::new(16, 9),
            color_space: ColorSpace::BT709,
            color_range: ColorRange::Unspecified,
            color_primaries: ColorPrimaries::BT709,
            color_transfer_characteristic: ColorTransferCharacteristic::BT709,
            chroma_location: ChromaLocation::Unspecified,
            references: 0,
            intra_dc_precision: 0,
            profile: "Main".to_string(),
            level: "4.0".to_string(),
            codec_name: codec_name.to_string(),
        },
        metadata: std::collections::HashMap::new(),
    })
}

pub(super) fn make_audio_stream(
    index: usize,
    language: &str,
    title: &str,
    channels: u16,
    sample_rate: u32,
    bit_rate: u64,
    codec_name: &str,
) -> UtilStreamMetadata {
    UtilStreamMetadata::Audio(UtilAudioStream {
        index,
        time_base: Ratio::new(1, 1000),
        start_time: 0,
        duration: 1_000_000,
        frames: 0,
        disposition: Disposition::default(),
        discard: Discard::Default,
        rate: None,
        codec_id: CodecId::AAC,
        audio: AudioMetadata {
            bit_rate,
            max_rate: 0,
            delay: 0,
            rate: sample_rate,
            channels,
            format: SampleFormat::None,
            frames: 0,
            align: 0,
            channel_layout: ChannelLayout {
                channels,
                description: None,
            },
            codec_name: codec_name.to_string(),
            profile: "LC".to_string(),
            title: title.to_string(),
            language: language.to_string(),
        },
        metadata: std::collections::HashMap::new(),
    })
}

pub(super) fn make_subtitle_stream(
    index: usize,
    language: Option<&str>,
    title: Option<&str>,
) -> UtilStreamMetadata {
    let mut metadata = std::collections::HashMap::new();
    if let Some(lang) = language {
        metadata.insert("language".to_string(), lang.to_string());
    }
    if let Some(t) = title {
        metadata.insert("title".to_string(), t.to_string());
    }
    UtilStreamMetadata::Subtitle(UtilSubtitleStream {
        index,
        time_base: Ratio::new(1, 1000),
        start_time: 0,
        duration: 1_000_000,
        disposition: Disposition::default(),
        discard: Discard::Default,
        codec_id: CodecId::SUBRIP,
        codec_name: "subrip".to_string(),
        metadata,
    })
}

/// Override a stream's disposition flags after construction, so the
/// `make_*_stream` builders above don't need a `disposition` parameter
/// threaded through every existing call site.
pub(super) fn with_disposition(
    mut stream: UtilStreamMetadata,
    default: bool,
    forced: bool,
) -> UtilStreamMetadata {
    let disposition = Disposition::for_test(default, forced);
    match &mut stream {
        UtilStreamMetadata::Video(v) => v.disposition = disposition,
        UtilStreamMetadata::Audio(a) => a.disposition = disposition,
        UtilStreamMetadata::Subtitle(s) => s.disposition = disposition,
    }
    stream
}

/// Override a video stream's color transfer characteristic after
/// construction, for HDR-detection tests.
pub(super) fn with_transfer_characteristic(
    mut stream: UtilStreamMetadata,
    transfer: ColorTransferCharacteristic,
) -> UtilStreamMetadata {
    if let UtilStreamMetadata::Video(v) = &mut stream {
        v.video.color_transfer_characteristic = transfer;
    }
    stream
}

pub(super) fn make_stream_file_metadata(streams: Vec<UtilStreamMetadata>) -> VideoFileMetadata {
    VideoFileMetadata {
        file_path: PathBuf::from("test.mp4"),
        metadata: Default::default(),
        best_video_stream: None,
        best_audio_stream: None,
        best_subtitle_stream: None,
        duration: 1_000_000,
        streams,
        format_name: "mp4".to_string(),
        format_long_name: "MPEG-4".to_string(),
        file_size: 1024,
        bit_rate: 1000,
        probe_score: 100,
        chapters: Vec::new(),
    }
}

pub(super) fn make_video_metadata() -> VideoFileMetadata {
    VideoFileMetadata {
        file_path: PathBuf::from("test"),
        metadata: std::collections::HashMap::default(),
        best_video_stream: None,
        best_audio_stream: None,
        best_subtitle_stream: None,
        duration: 1_000_000,
        streams: vec![],
        format_name: "mp4".to_string(),
        format_long_name: "MPEG-4".to_string(),
        file_size: 1024,
        bit_rate: 1000,
        probe_score: 100,
        chapters: Vec::new(),
    }
}

pub(super) async fn make_library_in_tempdir(
    lib_repo: &InMemoryLibraryRepository,
    dir: &TempDir,
) -> Library {
    lib_repo
        .create(CreateLibrary {
            name: "Test Library".to_string(),
            root_path: dir.path().to_path_buf(),
            description: None,
        })
        .await
        .unwrap()
}
