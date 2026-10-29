//! Real-media coverage for [`VideoFileMetadata::from_library_file`].
//!
//! The rest of the probe suite only exercises enum conversions
//! ([`super::color`], [`super::format`]). This module drives `from_library_file`
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
//! Live synthesis is deliberately video-only: the probe is agnostic to how
//! a file was produced, so audio-stream probing is covered just as well (and
//! far more robustly) by the committed AAC/Opus fixtures than by hand-rolling
//! a second native audio-encode + interleave pipeline here.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, Once};

use ffmpeg_next as ffmpeg;

use crate::library_file::LibraryFile;
use crate::probe::color::PixelFormat;
use crate::probe::media::CodecId;
use crate::probe::metadata::{MetadataError, StreamMetadata, VideoFileMetadata};

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

/// Cache directory for live-synthesised fixtures.
///
/// Under the build's `target/` rather than the system temp directory. The
/// fixtures are build artifacts -- they are produced by the FFmpeg this build
/// links -- so they belong with the other build artifacts: `cargo clean`
/// removes them, they cannot outlive a toolchain change into a later run, and
/// they do not consume a shared (often `tmpfs`, often small) `/tmp`.
fn synth_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("probe-fixtures");
    std::fs::create_dir_all(&dir).expect("create synth fixture dir");
    dir
}

/// Serialises fixture synthesis so parallel tests generate each file once.
///
/// This and [`FFMPEG_INIT`] are deliberately process-global. `ffmpeg::init`
/// mutates process-wide state and must run exactly once whatever the test
/// order; encoding is expensive and produces a file whose content depends only
/// on the codec, so doing it per test would buy nothing but wall-clock. The
/// synthesised path is published by an atomic rename, so a test never observes
/// a half-written fixture.
static SYNTH_LOCK: Mutex<()> = Mutex::new(());

/// Return the path to a live-synthesised fixture, generating it exactly once
/// and caching it for the rest of this build. `name` carries the container
/// extension (`.mp4`/`.mkv`) that the muxer is inferred from.
fn synth_fixture(name: &str, codec_id: ffmpeg::codec::Id) -> PathBuf {
    synth_fixture_with(name, codec_id, 25, &[])
}

/// [`synth_fixture`] for a file `nb_frames` long with `chapters`. `name`
/// must be unique to its arguments: it is the cache key.
fn synth_fixture_with(
    name: &str,
    codec_id: ffmpeg::codec::Id,
    nb_frames: i64,
    chapters: &[SynthChapter],
) -> PathBuf {
    synth_cached(name, |tmp| {
        encode_video_with(tmp, codec_id, nb_frames, chapters)
    })
}

/// The live-synthesised fixture `name`, made by `encode` -- which writes the
/// path it is given -- once for the build. `name` is the cache key, and
/// carries the container extension the muxer is inferred from.
fn synth_cached(name: &str, encode: impl FnOnce(&Path) -> Result<(), ffmpeg::Error>) -> PathBuf {
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
        encode(&tmp).expect("synthesise fixture");
        std::fs::rename(&tmp, &path).expect("publish synth fixture");
    }
    path
}

/// A chapter of a synthesised file: start and end in milliseconds, and its
/// title.
type SynthChapter = (i64, i64, &'static str);

/// Encode a tiny (64x64) video-only file with a native FFmpeg encoder.
///
/// Verbose by design: an explicit send-frame / drain-packet loop mirrors
/// ffmpeg-next's `transcode-x264` example rather than hiding the muxing
/// bookkeeping behind clever abstractions.
///
/// `nb_frames` long at 25 frames a second (25 is about a second), with
/// `chapters` written to the container.
fn encode_video_with(
    path: &Path,
    codec_id: ffmpeg::codec::Id,
    nb_frames: i64,
    chapters: &[SynthChapter],
) -> Result<(), ffmpeg::Error> {
    encode_video_from(path, codec_id, nb_frames, chapters, 0, fill_frame)
}

/// [`encode_video_with`], its first frame stamped `first_pts` (in frames)
/// rather than 0, and each frame `i` painted by `fill(frame, i)`.
fn encode_video_from(
    path: &Path,
    codec_id: ffmpeg::codec::Id,
    nb_frames: i64,
    chapters: &[SynthChapter],
    first_pts: i64,
    fill: fn(&mut ffmpeg::frame::Video, i64),
) -> Result<(), ffmpeg::Error> {
    use ffmpeg::{Packet, Rational, codec, encoder, format, frame};

    const W: u32 = 64;
    const H: u32 = 64;
    const FPS: i32 = 25;
    let enc_tb = Rational(1, FPS);

    let mut octx = format::output(&path)?;
    for (id, (start_ms, end_ms, title)) in chapters.iter().enumerate() {
        octx.add_chapter(id as i64 + 1, Rational(1, 1000), *start_ms, *end_ms, title)?;
    }
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

    for i in 0..nb_frames {
        let mut frame = frame::Video::new(format::Pixel::YUV420P, W, H);
        fill(&mut frame, i);
        frame.set_pts(Some(first_pts + i));
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

/// Probe `path` as the indexer does: opened beneath its folder with no link
/// followed, and read from that handle alone.
fn probe(path: &Path) -> Result<VideoFileMetadata, MetadataError> {
    let root = path.parent().expect("a file has a folder");
    let file = LibraryFile::open(root, path).expect("open the file to probe");
    VideoFileMetadata::from_library_file(file)
}

fn probe_committed(name: &str) -> VideoFileMetadata {
    init_ffmpeg();
    probe(&committed_fixture(name))
        .unwrap_or_else(|e| panic!("probing committed fixture {name} failed: {e}"))
}

// ---------------------------------------------------------------------------
// Live-synthesised fixtures (native encoders in the linked build)
// ---------------------------------------------------------------------------

#[test]
fn synth_mpeg4_mp4() {
    let path = synth_fixture("mpeg4.mp4", ffmpeg::codec::Id::MPEG4);
    let meta = probe(&path).expect("probe synth mpeg4 mp4");
    assert_video_fixture(&meta, CodecId::MPEG4);
    assert!(meta.format_name.contains("mp4") || meta.format_name.contains("mov"));
}

#[test]
fn synth_mpeg2video_mkv() {
    let path = synth_fixture("mpeg2video.mkv", ffmpeg::codec::Id::MPEG2VIDEO);
    let meta = probe(&path).expect("probe synth mpeg2video mkv");
    assert_video_fixture(&meta, CodecId::MPEG2VIDEO);
    assert!(meta.format_name.contains("matroska"));
}

/// The chapters of the three-second fixture below, as written.
const CHAPTERS: &[SynthChapter] = &[
    (0, 1_000, "Opening"),
    (1_000, 2_500, "Part A"),
    (2_500, 3_000, "Ending"),
];

/// Three seconds of MPEG-2 in Matroska, with [`CHAPTERS`].
fn chaptered_fixture() -> PathBuf {
    synth_fixture_with(
        "mpeg2video-3s-chapters.mkv",
        ffmpeg::codec::Id::MPEG2VIDEO,
        75,
        CHAPTERS,
    )
}

#[test]
fn chapters_are_read_from_the_container_in_milliseconds() {
    use crate::probe::metadata::ChapterMetadata;

    let meta = probe(&chaptered_fixture()).expect("probe the chaptered fixture");
    assert_eq!(
        meta.chapters,
        CHAPTERS
            .iter()
            .map(|(start_ms, end_ms, title)| ChapterMetadata {
                start_ms: *start_ms as u64,
                end_ms: *end_ms as u64,
                title: Some((*title).to_owned()),
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_file_without_chapters_reads_none() {
    let path = synth_fixture("mpeg2video.mkv", ffmpeg::codec::Id::MPEG2VIDEO);
    assert!(probe(&path).expect("probe").chapters.is_empty());
}

/// The sheets are written to their own folder and nowhere else: the
/// library the file is read from is left byte for byte as it was (FR-202).
#[test]
fn trickplay_sheets_tile_the_files_frames_into_jpegs() {
    use crate::library_file::test_support::snapshot;
    use crate::probe::trickplay::{TrickplaySpec, generate_sheets};

    let library = tempfile::tempdir().expect("a library");
    let path = library.path().join("Clip (2020).mkv");
    std::fs::copy(chaptered_fixture(), &path).expect("copy the fixture into the library");
    let out = tempfile::tempdir().expect("an output directory");
    let file = LibraryFile::open(library.path(), &path).expect("open the fixture");
    // A tile every 1.2 seconds: the fixture's 75 frames at 25 fps last
    // 2.96 seconds to its last frame's start and 3 to its end, so whichever
    // a muxer records as its duration it starts exactly three intervals.
    let spec = TrickplaySpec {
        interval_ms: 1_200,
        tile_width: 32,
        columns: 2,
        rows: 1,
        max_tiles: 100,
    };
    let before = snapshot(library.path());

    let layout = generate_sheets(file, out.path(), &spec, &AtomicBool::new(false))
        .expect("sheets are made")
        .expect("a video file has a layout");

    // A 64x64 picture is a square tile; three tiles, two to a sheet.
    assert_eq!((layout.tile_width, layout.tile_height), (32, 32));
    assert_eq!(layout.tile_count, 3);
    assert_eq!(layout.sheet_count(), 2);
    let mut written: Vec<String> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();
    assert_eq!(written, ["0.jpg", "1.jpg"]);
    assert_eq!(snapshot(library.path()), before, "the library is unchanged");

    // Each sheet is a JPEG of the whole grid: two tiles across, one down.
    for name in &written {
        let sheet = probe(&out.path().join(name)).expect("a sheet is an image FFmpeg reads");
        let idx = sheet.best_video_stream.expect("a sheet is a picture");
        match &sheet.streams[idx] {
            StreamMetadata::Video(v) => {
                assert_eq!(v.video.codec_name, "mjpeg", "{name}");
                assert_eq!((v.video.width, v.video.height), (64, 32), "{name}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn trickplay_of_a_file_that_is_not_media_is_an_error_and_writes_nothing() {
    use crate::probe::trickplay::{TrickplaySpec, generate_sheets};

    init_ffmpeg();
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("noise.mkv");
    std::fs::write(&path, [0x5a_u8; 4096]).unwrap();
    let out = tempfile::tempdir().expect("an output directory");
    let file = LibraryFile::open(dir.path(), &path).expect("open the file");

    let error = generate_sheets(
        file,
        out.path(),
        &TrickplaySpec::default(),
        &AtomicBool::new(false),
    )
    .expect_err("noise is not media");
    // Recorded as giving no sheets, rather than retried every pass.
    assert!(error.is_content_fault(), "{error:?}");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

#[test]
fn trickplay_cancelled_before_a_tile_stops_and_writes_nothing() {
    use crate::probe::trickplay::{TrickplayError, TrickplaySpec, generate_sheets};

    let path = chaptered_fixture();
    let out = tempfile::tempdir().expect("an output directory");
    let root = path.parent().expect("a file has a folder");
    let file = LibraryFile::open(root, &path).expect("open the fixture");

    let error = generate_sheets(
        file,
        out.path(),
        &TrickplaySpec::default(),
        &AtomicBool::new(true),
    )
    .expect_err("cancelled");

    assert!(matches!(error, TrickplayError::Cancelled), "{error:?}");
    // A cancellation says nothing about the file: it is tried again.
    assert!(!error.is_content_fault());
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

/// Six seconds of MPEG-2 in MPEG-TS whose timestamps start at ten seconds,
/// as a broadcast recording's do; frame `i` is a flat grey of luma `16 + i`,
/// so a tile's brightness says which frame it shows.
fn offset_start_fixture() -> PathBuf {
    synth_cached("mpeg2video-6s-start10s-ramp.ts", |tmp| {
        encode_video_from(
            tmp,
            ffmpeg::codec::Id::MPEG2VIDEO,
            150,
            &[],
            250,
            fill_flat_ramp,
        )
    })
}

/// Paint frame `i` a flat grey of limited-range luma `16 + i`.
fn fill_flat_ramp(frame: &mut ffmpeg::frame::Video, i: i64) {
    let luma = u8::try_from(16 + i).expect("a ramp within luma's range");
    for (plane, value) in [(0, luma), (1, 128), (2, 128)] {
        frame.data_mut(plane).fill(value);
    }
}

/// The luma plane of the JPEG at `path`, decoded: its width, height and
/// rows of `width` bytes.
fn decode_luma(path: &Path) -> (usize, usize, Vec<u8>) {
    let mut input = ffmpeg::format::input(path).expect("open the sheet");
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .expect("a sheet is a picture");
    let index = stream.index();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .unwrap()
        .decoder()
        .video()
        .unwrap();
    let mut frame = ffmpeg::frame::Video::empty();
    for (stream, packet) in input.packets() {
        if stream.index() != index {
            continue;
        }
        decoder.send_packet(&packet).unwrap();
        if decoder.receive_frame(&mut frame).is_ok() {
            break;
        }
    }
    let _ = decoder.send_eof();
    if frame.width() == 0 {
        decoder.receive_frame(&mut frame).expect("a sheet decodes");
    }
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let stride = frame.stride(0);
    let plane = frame.data(0);
    let rows = (0..height)
        .flat_map(|row| plane[row * stride..row * stride + width].iter().copied())
        .collect();
    (width, height, rows)
}

#[test]
fn trickplay_tiles_are_timed_from_the_files_start_not_timestamp_zero() {
    use crate::probe::trickplay::{TrickplaySpec, generate_sheets};

    let path = offset_start_fixture();
    // The fixture really does start late: its video's first timestamp is
    // ten seconds in.
    let meta = probe(&path).expect("probe the offset fixture");
    match &meta.streams[meta.best_video_stream.expect("a video stream")] {
        StreamMetadata::Video(v) => {
            let start_s = v.start_time as f64 * f64::from(*v.time_base.numer())
                / f64::from(*v.time_base.denom());
            assert!(start_s >= 9.9, "the fixture starts at {start_s}s");
        }
        other => panic!("{other:?}"),
    }

    let out = tempfile::tempdir().expect("an output directory");
    let root = path.parent().expect("a file has a folder");
    let file = LibraryFile::open(root, &path).expect("open the fixture");
    let spec = TrickplaySpec {
        interval_ms: 2_000,
        tile_width: 32,
        columns: 3,
        rows: 1,
        max_tiles: 3,
    };
    let layout = generate_sheets(file, out.path(), &spec, &AtomicBool::new(false))
        .expect("sheets are made")
        .expect("a video file has a layout");
    assert_eq!(layout.tile_count, 3);

    let (width, height, luma) = decode_luma(&out.path().join("0.jpg"));
    assert_eq!((width, height), (96, 32));
    // The sheet is full-range: limited-range luma `16 + i` reads `i * 255 / 219`.
    let full_range = |frame: i64| frame as f64 * 255.0 / 219.0;
    for tile in 0..3_usize {
        let pixels: Vec<u8> = (0..height)
            .flat_map(|row| {
                let from = row * width + tile * 32;
                luma[from..from + 32].iter().copied()
            })
            .collect();
        let mean = pixels.iter().map(|&p| f64::from(p)).sum::<f64>() / pixels.len() as f64;
        // Tile `n` is two seconds -- fifty frames -- into the file. The frame
        // shown is the keyframe at or before that, within one group of
        // pictures (12 frames); a tile timed from timestamp zero instead
        // fails every seek and shows one of the first frames read.
        let at = 50 * tile as i64;
        let (low, high) = (
            full_range((at - 12).max(0)) - 8.0,
            full_range(at + 12) + 8.0,
        );
        assert!(
            (low..=high).contains(&mean),
            "tile {tile}: mean luma {mean:.1}, wanted {low:.1}..={high:.1}"
        );
    }
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
        assert_eq!(v.video.codec_name, "h264");
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
    // The recorded codec name is FFmpeg's own, "hevc" (issue #189).
    let idx = meta.best_video_stream.unwrap();
    if let StreamMetadata::Video(v) = &meta.streams[idx] {
        assert_eq!(v.video.codec_name, "hevc");
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

    let result = probe(&path);
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
    let outcome = probe(&path);
    // Reaching here at all proves it did not panic/abort. In practice a
    // truncated MP4 parses Ok with degraded metadata (container recognised,
    // stream parameters incomplete); an Err is equally acceptable.
    if let Ok(meta) = outcome {
        // If it parsed, the header we kept should still describe MP4.
        assert!(meta.format_name.contains("mp4") || meta.format_name.contains("mov"));
    }
}

// ---------------------------------------------------------------------------
// Probing a handle reads what probing the path did (issue #238)
// ---------------------------------------------------------------------------

/// What each committed fixture probed as when FFmpeg still opened the path
/// itself (`ffmpeg::format::input`), recorded from that build before the
/// probe moved to reading the opened handle: container, duration, overall
/// bitrate, probe score, size and stream count. Reading the handle through a
/// custom I/O context must change none of it -- a file indexed before would
/// otherwise read as reprobed with different metadata.
#[test]
fn a_handle_probes_as_its_path_did() {
    let before: [(&str, &str, i64, i64, i32, u64, usize); 8] = [
        ("av1.mkv", "matroska,webm", 600_000, 30_040, 100, 2_253, 1),
        (
            "av1.mp4",
            "mov,mp4,m4a,3gp,3g2,mj2",
            600_000,
            26_666,
            100,
            2_000,
            1,
        ),
        ("av1.webm", "matroska,webm", 600_000, 29_133, 100, 2_185, 1),
        (
            "h264_aac.mp4",
            "mov,mp4,m4a,3gp,3g2,mj2",
            600_000,
            82_506,
            100,
            6_188,
            2,
        ),
        ("h264.mkv", "matroska,webm", 600_000, 45_973, 100, 3_448, 1),
        ("hevc.mkv", "matroska,webm", 600_000, 53_226, 100, 3_992, 1),
        (
            "hevc.mp4",
            "mov,mp4,m4a,3gp,3g2,mj2",
            600_000,
            57_493,
            100,
            4_312,
            1,
        ),
        (
            "vp9_opus.webm",
            "matroska,webm",
            608_000,
            62_973,
            100,
            4_786,
            2,
        ),
    ];
    for (name, format_name, duration, bit_rate, probe_score, file_size, streams) in before {
        let meta = probe_committed(name);
        assert_eq!(meta.format_name, format_name, "{name}");
        assert_eq!(meta.duration, duration, "{name}");
        assert_eq!(meta.bit_rate, bit_rate, "{name}");
        assert_eq!(meta.probe_score, probe_score, "{name}");
        assert_eq!(meta.file_size, file_size, "{name}");
        assert_eq!(meta.streams.len(), streams, "{name}");
        assert_eq!(meta.file_path, committed_fixture(name), "{name}");
    }
}

/// A container whose index comes after its media -- an MP4 written without
/// `faststart`, its `moov` beyond the probe's 32 KiB I/O buffer -- is probed
/// through the handle as FFmpeg probed the path. FFmpeg seeks the handle:
/// past the buffer to the index and back, and to its end for its size, from
/// which it reckons the overall bitrate. Through a handle it could only read
/// forward it would find the streams all the same, by reading through the
/// media, but not the size -- the bitrate would read as zero.
#[test]
fn a_container_indexed_at_its_end_is_probed_by_seeking_the_handle() {
    let name = "h264_aac_moov_at_end.mp4";
    let fixture = std::fs::read(committed_fixture(name)).unwrap();
    let moov = fixture
        .windows(4)
        .position(|window| window == b"moov")
        .expect("the fixture has a moov box");
    assert!(
        moov > 32 * 1024,
        "the index lies beyond the I/O buffer, at {moov}"
    );

    let meta = probe_committed(name);
    assert_eq!(meta.format_name, "mov,mp4,m4a,3gp,3g2,mj2");
    assert_eq!(meta.file_size, fixture.len() as u64);
    assert_eq!(meta.streams.len(), 2);
    match &meta.streams[meta.best_video_stream.expect("a video stream")] {
        StreamMetadata::Video(v) => {
            assert_eq!((v.video.width, v.video.height), (160, 120));
            assert_eq!(v.codec_id, CodecId::H264);
        }
        other => panic!("best video stream index resolved to {other:?}"),
    }
    assert_audio_fixture(&meta, CodecId::AAC);
    assert!(
        (3.9..=4.1).contains(&meta.duration_seconds()),
        "duration {}",
        meta.duration_seconds()
    );
    // FFmpeg's overall bitrate: the size it sought to, over the duration.
    let size_bits = fixture.len() as i64 * 8;
    assert_eq!(meta.bit_rate, size_bits * 1_000_000 / meta.duration);
}

/// The probe reads from wherever the handle stands: a handle the hash has
/// just read to its end is probed from its start, as the indexer hands it
/// over.
#[test]
fn a_handle_read_to_its_end_is_probed_from_its_start() {
    init_ffmpeg();
    let path = committed_fixture("h264_aac.mp4");
    let file = LibraryFile::open(path.parent().unwrap(), &path).unwrap();
    let mut hashed = file.try_clone_file().unwrap();
    std::io::copy(&mut hashed, &mut std::io::sink()).unwrap();

    let meta = VideoFileMetadata::from_library_file(file).expect("probe after a hash");
    assert_video_fixture(&meta, CodecId::H264);
    assert_audio_fixture(&meta, CodecId::AAC);
}

/// A file that names another by URL -- here an HLS playlist naming a video
/// segment outside its folder -- is probed from its own bytes only (issue
/// #246): FFmpeg opens nothing the handle does not hold, so the segment's
/// stream is never read into the playlist's metadata. The same segment
/// probed directly has the stream, so its absence is the refusal.
#[test]
fn a_file_naming_another_by_url_is_probed_from_its_own_bytes_only() {
    let segment = synth_fixture("mpeg2video.ts", ffmpeg::codec::Id::MPEG2VIDEO);
    assert_video_fixture(
        &probe(&segment).expect("probe the segment"),
        CodecId::MPEG2VIDEO,
    );
    let dir = tempfile::TempDir::new().unwrap();
    let playlist = dir.path().join("playlist.m3u8");
    std::fs::write(
        &playlist,
        format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:1.0,\n{}\n#EXT-X-ENDLIST\n",
            segment.display()
        ),
    )
    .unwrap();

    // A playlist with no segment to read may fail to probe at all; one that
    // probes has no stream.
    if let Ok(meta) = probe(&playlist) {
        assert!(
            meta.best_video_stream.is_none() && meta.streams.is_empty(),
            "nothing of the segment is read: {:?}",
            meta.streams
        );
    }
}
