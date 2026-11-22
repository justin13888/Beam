//! [`fingerprint_file`] against real files, demuxed and decoded by the
//! linked FFmpeg: WAV files written here, since a WAV is a header and the
//! samples, so a test can say exactly where each episode's theme lies.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use beam_domain::models::chapter::MarkerKind;

use super::*;
use crate::probe::fingerprint::detect_markers;
use crate::probe::fingerprint::fingerprint_tests::programme;

/// Write `left` and `right` as a 16-bit stereo WAV at `rate`.
fn write_wav(path: &Path, rate: u32, left: &[f32], right: &[f32]) {
    let frames = left.len().min(right.len());
    let data_len = u32::try_from(frames * 4).expect("a small file");
    let mut bytes = Vec::with_capacity(44 + frames * 4);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2_u16.to_le_bytes()); // channels
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&4_u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    let pcm = |sample: f32| ((sample.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes();
    for i in 0..frames {
        bytes.extend_from_slice(&pcm(left[i]));
        bytes.extend_from_slice(&pcm(right[i]));
    }
    std::fs::write(path, bytes).expect("write the WAV");
}

/// `seconds` of `signal` -- made at [`SAMPLE_RATE`] -- at `rate`, each
/// sample held for its period: enough to resample it back.
fn at_rate(signal: &[f32], rate: u32) -> Vec<f32> {
    let len = signal.len() * rate as usize / SAMPLE_RATE as usize;
    (0..len)
        .map(|i| signal[i * SAMPLE_RATE as usize / rate as usize])
        .collect()
}

/// An episode of `seconds`: its own programme throughout, with `theme`
/// laid over it at `theme_at` and `credits` at `credits_at` (seconds).
fn episode(seed: u64, seconds: f32, theme: (&[f32], f32), credits: (&[f32], f32)) -> Vec<f32> {
    let mut audio: Vec<f32> = programme(seed, seconds).iter().map(|s| s * 0.3).collect();
    for (part, at) in [theme, credits] {
        let at = (at * SAMPLE_RATE as f32) as usize;
        audio[at..at + part.len()].copy_from_slice(part);
    }
    audio
}

fn init() {
    crate::probe::init().expect("ffmpeg init");
    ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Quiet);
}

/// Two episodes, as stereo WAVs at a rate that is not the fingerprint's,
/// sharing a twenty-second theme near their start and twenty seconds of
/// credits near their end, each at its own place: each is given the intro
/// and credits where its own copy lies.
#[test]
fn two_wav_episodes_are_given_the_intro_and_credits_they_share() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let theme = programme(500, 20.0);
    let credits = programme(600, 20.0);
    let rate = 16_000;
    let write = |name: &str, audio: &[f32]| {
        let audio = at_rate(audio, rate);
        let path = dir.path().join(name);
        write_wav(&path, rate, &audio, &audio);
        path
    };
    let a = write(
        "a.wav",
        &episode(1, 150.0, (&theme, 5.0), (&credits, 120.0)),
    );
    let b = write(
        "b.wav",
        &episode(2, 150.0, (&theme, 15.0), (&credits, 125.0)),
    );
    let cancel = AtomicBool::new(false);
    let print = |path: &Path| {
        fingerprint_file(
            LibraryFile::open(dir.path(), path).unwrap(),
            &FingerprintSpec::default(),
            &cancel,
        )
        .unwrap()
        .expect("a WAV has audio")
    };
    let (a, b) = (print(&a), print(&b));

    // 150 s: the first 50 heard, and the last 37.5.
    assert_eq!(a.tail_start_ms, 112_500);

    let markers = detect_markers(&a, &[&b]);
    let near = |got: u64, want: u64| got.abs_diff(want) <= 500;
    assert_eq!(markers.len(), 2, "{markers:?}");
    assert_eq!(markers[0].kind, MarkerKind::Intro);
    assert!(
        near(markers[0].start_ms, 5_000) && near(markers[0].end_ms, 25_000),
        "{markers:?}"
    );
    assert_eq!(markers[1].kind, MarkerKind::Credits);
    assert!(
        near(markers[1].start_ms, 120_000) && near(markers[1].end_ms, 140_000),
        "{markers:?}"
    );
}

#[test]
fn a_file_that_is_not_media_is_a_content_fault() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.wav");
    std::fs::write(&path, b"this is not a WAV file at all").unwrap();

    let error = fingerprint_file(
        LibraryFile::open(dir.path(), &path).unwrap(),
        &FingerprintSpec::default(),
        &AtomicBool::new(false),
    )
    .expect_err("nothing to demux");

    assert!(error.is_content_fault(), "{error}");
}

#[test]
fn listening_cancelled_stops_before_the_first_packet() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.wav");
    let audio = programme(1, 10.0);
    write_wav(&path, SAMPLE_RATE, &audio, &audio);

    let error = fingerprint_file(
        LibraryFile::open(dir.path(), &path).unwrap(),
        &FingerprintSpec::default(),
        &AtomicBool::new(true),
    )
    .expect_err("cancelled");

    assert!(matches!(error, FingerprintError::Cancelled), "{error}");
    assert!(!error.is_content_fault());
}
