//! Real-media coverage for [`VideoFileMetadata::from_path`].
//!
//! The rest of the probe suite only exercises enum conversions
//! ([`super::color`], [`super::format`]). This module drives `from_path`
//! against *actual demuxed containers* so the FFmpeg-facing metadata path
//! (stream enumeration, decoder-context opening, codec/color/duration
//! extraction) is covered end-to-end without any external service. See
//! GitHub issue #92.
//!
//! Two fixture sources:
//!
//! 1. **Live-synthesised** at test time with the linked (vendored LGPL)
//!    FFmpeg, for codecs that build can encode natively (`mpeg4`,
//!    `mpeg2video`). Cached across tests in a per-build temp dir.
//! 2. **Committed** tiny binaries under `tests/fixtures/` for the modern
//!    codecs the LGPL/CI builds can *decode* but not *encode* (H.264, HEVC,
//!    AV1, VP9, plus AAC/Opus audio). See that directory's `README.md`.
//!
//! Live synthesis is deliberately video-only: `from_path` is agnostic to how
//! a file was produced, so audio-stream probing is covered just as well (and
//! far more robustly) by the committed AAC/Opus fixtures than by hand-rolling
//! a second native audio-encode + interleave pipeline here.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use ffmpeg_next as ffmpeg;

use crate::probe::color::PixelFormat;
use crate::probe::media::CodecId;
use crate::probe::metadata::{StreamMetadata, VideoFileMetadata};

static FFMPEG_INIT: Once = Once::new();

/// Idempotently initialise the FFmpeg bindings. Production wires this via
/// [`crate::probe::init`] (see `beam-server` startup); tests must do the same
/// before touching any probing API.
fn init_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        crate::probe::init().expect("ffmpeg init failed");
        // The corrupt/truncated cases intentionally feed FFmpeg garbage; keep
        // its internal logging off the test output.
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Quiet);
    });
}

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// Path to a committed fixture under `beam-index/tests/fixtures/`.
fn committed_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Per-build cache directory for live-synthesised fixtures.
fn synth_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("beam-index-probe-fixtures");
    std::fs::create_dir_all(&dir).expect("create synth fixture dir");
    dir
}

/// Serialises fixture synthesis so parallel tests generate each file once.
static SYNTH_LOCK: Mutex<()> = Mutex::new(());

/// Return the path to a live-synthesised fixture, generating it exactly once
/// and caching it for the rest of this build. `name` carries the container
/// extension (`.mp4`/`.mkv`) that the muxer is inferred from.
fn synth_fixture(name: &str, codec_id: ffmpeg::codec::Id) -> PathBuf {
    init_ffmpeg();
    let path = synth_dir().join(name);

    let _guard = SYNTH_LOCK.lock().unwrap();
    let cached = std::fs::metadata(&path)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if !cached {
        // Write to a sibling temp path that keeps the container extension so
        // muxer inference still works, then rename into place atomically.
        let tmp = synth_dir().join(format!("partial-{name}"));
        encode_video(&tmp, codec_id).expect("synthesise fixture");
        std::fs::rename(&tmp, &path).expect("publish synth fixture");
    }
    path
}

/// Encode a tiny (64x64, ~1s) video-only file with a native FFmpeg encoder.
///
/// Verbose by design: an explicit send-frame / drain-packet loop mirrors
/// ffmpeg-next's `transcode-x264` example rather than hiding the muxing
/// bookkeeping behind clever abstractions.
fn encode_video(path: &Path, codec_id: ffmpeg::codec::Id) -> Result<(), ffmpeg::Error> {
    use ffmpeg::{Packet, Rational, codec, encoder, format, frame};

    const W: u32 = 64;
    const H: u32 = 64;
    const FPS: i32 = 25;
    const NB_FRAMES: i64 = 25; // ~1 second
    let enc_tb = Rational(1, FPS);

    let mut octx = format::output(&path)?;
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    let vcodec = encoder::find(codec_id).ok_or(ffmpeg::Error::EncoderNotFound)?;

    let mut enc = codec::context::Context::new_with_codec(vcodec)
        .encoder()
        .video()?;
    enc.set_width(W);
    enc.set_height(H);
    enc.set_format(format::Pixel::YUV420P);
    enc.set_time_base(enc_tb);
    enc.set_frame_rate(Some(Rational(FPS, 1)));
    enc.set_bit_rate(64_000);
    if global_header {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut enc = enc.open()?;

    {
        let mut ost = octx.add_stream(vcodec)?;
        ost.set_parameters(&enc);
        ost.set_time_base(enc_tb);
    }

    octx.write_header()?;
    let ost_tb = octx.stream(0).expect("output video stream").time_base();

    for i in 0..NB_FRAMES {
        let mut frame = frame::Video::new(format::Pixel::YUV420P, W, H);
        fill_frame(&mut frame, i);
        frame.set_pts(Some(i));
        enc.send_frame(&frame)?;

        let mut packet = Packet::empty();
        while enc.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            packet.rescale_ts(enc_tb, ost_tb);
            packet.write_interleaved(&mut octx)?;
        }
    }

    // Flush the encoder.
    enc.send_eof()?;
    let mut packet = Packet::empty();
    while enc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(0);
        packet.rescale_ts(enc_tb, ost_tb);
        packet.write_interleaved(&mut octx)?;
    }

    octx.write_trailer()?;
    Ok(())
}

/// Fill a YUV420P frame with a moving gradient (valid content for any encoder).
fn fill_frame(frame: &mut ffmpeg::frame::Video, i: i64) {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let shift = (i as usize) * 3;

    // Luma plane: gradient that drifts per frame.
    let ys = frame.stride(0);
    let yd = frame.data_mut(0);
    for y in 0..h {
        for x in 0..w {
            yd[y * ys + x] = (x + y + shift) as u8;
        }
    }

    // Chroma planes (half resolution): neutral grey.
    for plane in 1..3 {
        let cs = frame.stride(plane);
        let cd = frame.data_mut(plane);
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                cd[y * cs + x] = 128;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Assert the best video stream is 64x64 with the expected codec and a
/// positive duration.
fn assert_video_fixture(meta: &VideoFileMetadata, expected: CodecId) {
    let idx = meta.best_video_stream.expect("fixture has a video stream");
    match &meta.streams[idx] {
        StreamMetadata::Video(v) => {
            assert_eq!(v.video.width, 64, "width");
            assert_eq!(v.video.height, 64, "height");
            assert_eq!(v.codec_id, expected, "video codec");
        }
        other => panic!("best video stream index resolved to {other:?}"),
    }
    assert!(
        meta.duration_seconds() > 0.0,
        "file duration should be positive, got {}",
        meta.duration_seconds()
    );
}

/// Assert the best audio stream carries the expected codec.
fn assert_audio_fixture(meta: &VideoFileMetadata, expected: CodecId) {
    let idx = meta.best_audio_stream.expect("fixture has an audio stream");
    match &meta.streams[idx] {
        StreamMetadata::Audio(a) => assert_eq!(a.codec_id, expected, "audio codec"),
        other => panic!("best audio stream index resolved to {other:?}"),
    }
}

fn probe_committed(name: &str) -> VideoFileMetadata {
    init_ffmpeg();
    VideoFileMetadata::from_path(&committed_fixture(name))
        .unwrap_or_else(|e| panic!("probing committed fixture {name} failed: {e}"))
}

// ---------------------------------------------------------------------------
// Live-synthesised fixtures (native encoders in the linked build)
// ---------------------------------------------------------------------------

#[test]
fn synth_mpeg4_mp4() {
    let path = synth_fixture("mpeg4.mp4", ffmpeg::codec::Id::MPEG4);
    let meta = VideoFileMetadata::from_path(&path).expect("probe synth mpeg4 mp4");
    assert_video_fixture(&meta, CodecId::MPEG4);
    assert!(meta.format_name.contains("mp4") || meta.format_name.contains("mov"));
}

#[test]
fn synth_mpeg2video_mkv() {
    let path = synth_fixture("mpeg2video.mkv", ffmpeg::codec::Id::MPEG2VIDEO);
    let meta = VideoFileMetadata::from_path(&path).expect("probe synth mpeg2video mkv");
    assert_video_fixture(&meta, CodecId::MPEG2VIDEO);
    assert!(meta.format_name.contains("matroska"));
}

// ---------------------------------------------------------------------------
// Committed fixtures (modern codecs the build can decode but not encode)
// ---------------------------------------------------------------------------

#[test]
fn committed_h264_aac_mp4() {
    let meta = probe_committed("h264_aac.mp4");
    assert_video_fixture(&meta, CodecId::H264);
    assert_audio_fixture(&meta, CodecId::AAC);

    // Exercise the raw ffmpeg codec-name string path on at least one fixture.
    let idx = meta.best_video_stream.unwrap();
    if let StreamMetadata::Video(v) = &meta.streams[idx] {
        assert_eq!(v.video.codec_name, "H264");
    }
}

#[test]
fn committed_h264_mkv() {
    assert_video_fixture(&probe_committed("h264.mkv"), CodecId::H264);
}

// Real HEVC streams arrive from ffmpeg-next as `Id::HEVC` (never the
// `Id::H265` alias); these tests guard the mapping in `probe::media` that
// folds both into `CodecId::H265`.
#[test]
fn committed_hevc_mp4() {
    let meta = probe_committed("hevc.mp4");
    assert_video_fixture(&meta, CodecId::H265);
    // The raw ffmpeg codec-name string reads "HEVC" (Debug of Id::HEVC).
    let idx = meta.best_video_stream.unwrap();
    if let StreamMetadata::Video(v) = &meta.streams[idx] {
        assert_eq!(v.video.codec_name, "HEVC");
    }
}

#[test]
fn committed_hevc_mkv() {
    assert_video_fixture(&probe_committed("hevc.mkv"), CodecId::H265);
}

#[test]
fn committed_av1_mp4() {
    assert_video_fixture(&probe_committed("av1.mp4"), CodecId::AV1);
}

#[test]
fn committed_av1_mkv() {
    assert_video_fixture(&probe_committed("av1.mkv"), CodecId::AV1);
}

#[test]
fn committed_av1_webm() {
    let meta = probe_committed("av1.webm");
    assert_video_fixture(&meta, CodecId::AV1);
    assert!(meta.format_name.contains("webm") || meta.format_name.contains("matroska"));
}

#[test]
fn committed_vp9_opus_webm() {
    let meta = probe_committed("vp9_opus.webm");
    assert_video_fixture(&meta, CodecId::VP9);
    assert_audio_fixture(&meta, CodecId::OPUS);
}

// ---------------------------------------------------------------------------
// Color / pixel-format extraction on real streams
// ---------------------------------------------------------------------------

#[test]
fn real_pixel_format_and_color_fields() {
    // Every fixture is encoded as YUV420P (8-bit). Assert the color/format
    // conversion path produces real values from an actual decoded header.
    let meta = probe_committed("h264.mkv");
    let idx = meta.best_video_stream.unwrap();
    let StreamMetadata::Video(v) = &meta.streams[idx] else {
        panic!("expected video stream");
    };
    assert_eq!(v.video.format, PixelFormat::YUV420P);
    assert_eq!(v.video.bit_depth(), Some(8));
    // aspect_ratio came through the ffmpeg Rational conversion without error.
    assert_eq!(v.video.resolution().width, 64);
}

// ---------------------------------------------------------------------------
// Corrupt / truncated inputs must fail gracefully (never panic/abort)
// ---------------------------------------------------------------------------

#[test]
fn corrupt_random_bytes_mkv_returns_err() {
    init_ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.mkv");
    // Non-container random bytes: ffmpeg cannot identify a demuxer.
    std::fs::write(&path, [0x00, 0x11, 0x22, 0xde, 0xad, 0xbe, 0xef].repeat(64)).unwrap();

    let result = VideoFileMetadata::from_path(&path);
    assert!(
        result.is_err(),
        "random bytes must not parse as a container"
    );
}

#[test]
fn truncated_fixture_does_not_panic() {
    init_ffmpeg();
    // Copy the first ~40% of a valid MP4 to simulate an interrupted download.
    let full = std::fs::read(committed_fixture("h264_aac.mp4")).unwrap();
    let cut = (full.len() * 2) / 5;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.mp4");
    std::fs::write(&path, &full[..cut]).unwrap();

    // The contract is only "no panic / process stays healthy"; a truncated
    // moov-less MP4 may either error or return degraded-but-Ok metadata.
    let outcome = VideoFileMetadata::from_path(&path);
    // Reaching here at all proves it did not panic/abort. In practice a
    // truncated MP4 parses Ok with degraded metadata (container recognised,
    // stream parameters incomplete); an Err is equally acceptable.
    if let Ok(meta) = outcome {
        // If it parsed, the header we kept should still describe MP4.
        assert!(meta.format_name.contains("mp4") || meta.format_name.contains("mov"));
    }
}
