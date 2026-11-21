//! `insert_media_streams`: what a probed stream is persisted as.

use super::test_harness::*;
use super::*;
use crate::probe::color::ColorTransferCharacteristic;
use crate::probe::format::Disposition;
use crate::probe::media::CodecId;
use crate::probe::metadata::StreamMetadata as UtilStreamMetadata;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::MockFileRepository;
use beam_domain::repositories::library::MockLibraryRepository;
use beam_domain::repositories::movie::MockMovieRepository;
use beam_domain::repositories::show::MockShowRepository;
use beam_domain::repositories::stream::MockMediaStreamRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use num::rational::Ratio;

// ── insert_media_streams unit tests ────────────────────────────────────────

#[tokio::test]
async fn test_insert_video_stream_fields() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_video_stream(
        0,
        1920,
        1080,
        5_000_000,
        "h264",
        Some(Ratio::new(30, 1)),
    )]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert_eq!(result.unwrap(), 1);

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert_eq!(streams.len(), 1);

    let s = &streams[0];
    assert_eq!(
        s.stream_type,
        beam_domain::models::stream::StreamType::Video
    );
    assert_eq!(s.codec, "h264");
    assert_eq!(s.index, 0);

    if let beam_domain::models::stream::StreamMetadata::Video(v) = &s.metadata {
        assert_eq!(v.width, 1920);
        assert_eq!(v.height, 1080);
        assert_eq!(v.frame_rate, Some(30.0));
        assert_eq!(v.bit_rate, Some(5_000_000));
    } else {
        panic!("expected Video metadata");
    }
}

#[tokio::test]
async fn test_insert_video_stream_persists_sdr_color_metadata() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_video_stream(
        0, 1920, 1080, 5_000_000, "h264", None,
    )]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
        assert_eq!(v.color_space.as_deref(), Some("BT.709"));
        assert_eq!(v.color_range.as_deref(), Some("Unspecified"));
        assert_eq!(v.hdr_format, None);
    } else {
        panic!("expected Video metadata");
    }
}

#[tokio::test]
async fn test_insert_video_stream_smpte2084_persists_hdr10() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let stream = make_video_stream(0, 3840, 2160, 20_000_000, "hevc", None);
    let stream = with_transfer_characteristic(stream, ColorTransferCharacteristic::SMPTE2084);
    let metadata = make_stream_file_metadata(vec![stream]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
        assert_eq!(v.hdr_format.as_deref(), Some("HDR10"));
    } else {
        panic!("expected Video metadata");
    }
}

#[tokio::test]
async fn test_insert_video_stream_arib_std_b67_persists_hlg() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let stream = make_video_stream(0, 3840, 2160, 20_000_000, "hevc", None);
    let stream = with_transfer_characteristic(stream, ColorTransferCharacteristic::AribStdB67);
    let metadata = make_stream_file_metadata(vec![stream]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Video(v) = &streams[0].metadata {
        assert_eq!(v.hdr_format.as_deref(), Some("HLG"));
    } else {
        panic!("expected Video metadata");
    }
}

#[tokio::test]
async fn test_insert_audio_stream_with_language() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_audio_stream(
        0, "eng", "English", 2, 48_000, 128_000, "aac",
    )]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert_eq!(result.unwrap(), 1);

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert_eq!(streams.len(), 1);

    let s = &streams[0];
    assert_eq!(
        s.stream_type,
        beam_domain::models::stream::StreamType::Audio
    );
    assert_eq!(s.codec, "aac");

    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &s.metadata {
        assert_eq!(a.language, Some("eng".to_string()));
    } else {
        panic!("expected Audio metadata");
    }
}

#[tokio::test]
async fn test_insert_audio_stream_empty_language_becomes_none() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_audio_stream(
        0, "", "", 2, 48_000, 128_000, "aac",
    )]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
        assert_eq!(a.language, None);
        assert_eq!(a.title, None);
    } else {
        panic!("expected Audio metadata");
    }
}

#[tokio::test]
async fn test_insert_audio_stream_title_populated_or_none() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![
        make_audio_stream(0, "eng", "Director Commentary", 2, 48_000, 128_000, "aac"),
        make_audio_stream(1, "eng", "", 2, 48_000, 128_000, "aac"),
    ]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert_eq!(streams.len(), 2);

    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
        assert_eq!(a.title, Some("Director Commentary".to_string()));
    } else {
        panic!("expected Audio metadata");
    }
    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[1].metadata {
        assert_eq!(a.title, None);
    } else {
        panic!("expected Audio metadata");
    }
}

#[tokio::test]
async fn test_insert_audio_stream_channels_and_sample_rate() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_audio_stream(
        0, "eng", "", 6, 48_000, 448_000, "ac3",
    )]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
        assert_eq!(a.channels, 6);
        assert_eq!(a.sample_rate, 48_000);
    } else {
        panic!("expected Audio metadata");
    }
}

#[tokio::test]
async fn test_insert_audio_stream_default_and_forced_flags_persisted() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let default_track = with_disposition(
        make_audio_stream(0, "eng", "", 2, 48_000, 128_000, "aac"),
        true,
        false,
    );
    let commentary_track = with_disposition(
        make_audio_stream(1, "eng", "Commentary", 2, 48_000, 128_000, "aac"),
        false,
        false,
    );
    let metadata = make_stream_file_metadata(vec![default_track, commentary_track]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[0].metadata {
        assert!(a.is_default);
        assert!(!a.is_forced);
    } else {
        panic!("expected Audio metadata");
    }
    if let beam_domain::models::stream::StreamMetadata::Audio(a) = &streams[1].metadata {
        assert!(!a.is_default);
    } else {
        panic!("expected Audio metadata");
    }
}

#[tokio::test]
async fn test_insert_subtitle_stream_default_and_forced_flags_persisted() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let forced_track = with_disposition(
        make_subtitle_stream(0, Some("eng"), Some("Forced")),
        false,
        true,
    );
    let metadata = make_stream_file_metadata(vec![forced_track]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    if let beam_domain::models::stream::StreamMetadata::Subtitle(sub) = &streams[0].metadata {
        assert!(!sub.is_default);
        assert!(sub.is_forced);
    } else {
        panic!("expected Subtitle metadata");
    }
}

/// The stored codec is the prober's FFmpeg name, for an image-based
/// subtitle as for any other (#189): a PGS track used to read `Other(..)`.
/// A stream flagged for the deaf and hard of hearing keeps the flag.
#[tokio::test]
async fn test_insert_subtitle_stream_records_ffmpeg_codec_name_and_sdh() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let mut pgs = make_subtitle_stream(0, Some("eng"), None);
    if let UtilStreamMetadata::Subtitle(s) = &mut pgs {
        s.codec_id = CodecId::Other("hdmv_pgs_subtitle".to_string());
        s.codec_name = "hdmv_pgs_subtitle".to_string();
        s.disposition =
            Disposition::from(ffmpeg_next::format::stream::Disposition::HEARING_IMPAIRED);
    }
    let plain = make_subtitle_stream(1, Some("eng"), None);
    let metadata = make_stream_file_metadata(vec![pgs, plain]);

    service
        .insert_media_streams(file_id, &metadata)
        .await
        .unwrap();

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    let sdh = |stream: &beam_domain::models::MediaStream| match &stream.metadata {
        beam_domain::models::stream::StreamMetadata::Subtitle(sub) => sub.is_hearing_impaired,
        other => panic!("expected Subtitle metadata, got {other:?}"),
    };
    assert_eq!(streams[0].codec, "hdmv_pgs_subtitle");
    assert!(sdh(&streams[0]));
    assert_eq!(streams[1].codec, "subrip");
    assert!(!sdh(&streams[1]));
}

#[tokio::test]
async fn test_insert_subtitle_stream_fields() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![make_subtitle_stream(
        0,
        Some("eng"),
        Some("English SDH"),
    )]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert_eq!(result.unwrap(), 1);

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert_eq!(streams.len(), 1);

    let s = &streams[0];
    assert_eq!(
        s.stream_type,
        beam_domain::models::stream::StreamType::Subtitle
    );

    if let beam_domain::models::stream::StreamMetadata::Subtitle(sub) = &s.metadata {
        assert_eq!(sub.language, Some("eng".to_string()));
        assert_eq!(sub.title, Some("English SDH".to_string()));
    } else {
        panic!("expected Subtitle metadata");
    }
}

#[tokio::test]
async fn test_insert_mixed_streams_all_inserted() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![
        make_video_stream(0, 1920, 1080, 5_000_000, "h264", Some(Ratio::new(24, 1))),
        make_audio_stream(1, "eng", "English", 2, 48_000, 192_000, "aac"),
        make_audio_stream(2, "fra", "French", 2, 48_000, 128_000, "aac"),
        make_subtitle_stream(3, Some("eng"), Some("English")),
    ]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert_eq!(result.unwrap(), 4);

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert_eq!(streams.len(), 4);

    use beam_domain::models::stream::StreamType;
    assert_eq!(streams[0].stream_type, StreamType::Video);
    assert_eq!(streams[1].stream_type, StreamType::Audio);
    assert_eq!(streams[2].stream_type, StreamType::Audio);
    assert_eq!(streams[3].stream_type, StreamType::Subtitle);
}

#[tokio::test]
async fn test_insert_empty_streams_returns_zero() {
    let repo = Arc::new(InMemoryMediaStreamRepository::default());
    let service = make_service_with_stream_repo(Arc::clone(&repo));
    let file_id = Uuid::new_v4();

    let metadata = make_stream_file_metadata(vec![]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert_eq!(result.unwrap(), 0);

    let streams = repo.find_by_file_id(file_id).await.unwrap();
    assert!(streams.is_empty());
}

#[tokio::test]
async fn test_insert_streams_db_error_propagates() {
    let mut mock_stream_repo = MockMediaStreamRepository::new();
    mock_stream_repo
        .expect_insert_streams()
        .times(1)
        .returning(|_| Err(sea_orm::DbErr::Custom("simulated DB failure".to_string())));

    let service = LocalIndexService::new(
        Arc::new(MockLibraryRepository::new()),
        Arc::new(MockFileRepository::new()),
        Arc::new(MockMovieRepository::new()),
        Arc::new(MockShowRepository::new()),
        Arc::new(mock_stream_repo),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let file_id = Uuid::new_v4();
    let metadata = make_stream_file_metadata(vec![make_video_stream(
        0, 1280, 720, 2_000_000, "h264", None,
    )]);

    let result = service.insert_media_streams(file_id, &metadata).await;
    assert!(matches!(result, Err(IndexError::Db(_))));
}
