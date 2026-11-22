//! A video file's audio, decoded for skip-marker detection (issue #332).
//!
//! The opening and the ending of the file's main audio track are decoded,
//! mixed to mono, brought to [`SAMPLE_RATE`] and fingerprinted
//! ([`super::fingerprint`]); the spans a season's episodes share are found
//! from those. Only those two stretches are decoded -- the ending reached by
//! seeking -- so an episode costs a few seconds, not its running time.
//!
//! The file is read through its open handle and nothing else, as the probe
//! reads it ([`super::metadata::open_input`]), and nothing is written: never
//! beside the library's files (FR-202).

use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_next as ffmpeg;
use thiserror::Error;
use tracing::{debug, trace};

use crate::library_file::LibraryFile;
use crate::probe::fingerprint::{
    AudioFingerprint, Downsampler, FingerprintSpec, SAMPLE_RATE, fingerprint,
};
use crate::probe::metadata::{MetadataError, open_input};

#[derive(Debug, Error)]
pub enum FingerprintError {
    #[error("FFmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg::Error),
    #[error("failed to open the file: {0}")]
    Open(#[from] MetadataError),
    #[error("failed to read the file: {0}")]
    Read(std::io::Error),
    #[error("listening to the file was cancelled")]
    Cancelled,
}

impl FingerprintError {
    /// Whether the error is a property of the file's content -- it is not
    /// media FFmpeg can demux, or its audio has no decoder -- and so will
    /// recur until the content changes. Anything else (a read error from a
    /// flaky mount, a cancellation) says nothing about the file, and is worth
    /// another try.
    pub fn is_content_fault(&self) -> bool {
        fn of_content(error: &ffmpeg::Error) -> bool {
            matches!(
                error,
                ffmpeg::Error::InvalidData
                    | ffmpeg::Error::DemuxerNotFound
                    | ffmpeg::Error::DecoderNotFound
                    | ffmpeg::Error::StreamNotFound
            )
        }
        match self {
            FingerprintError::Ffmpeg(error) => of_content(error),
            FingerprintError::Open(MetadataError::FfmpegError(error)) => of_content(error),
            FingerprintError::Open(MetadataError::InvalidMetadata(_)) => true,
            FingerprintError::Open(MetadataError::UnknownError(_))
            | FingerprintError::Read(_)
            | FingerprintError::Cancelled => false,
        }
    }
}

/// A demuxer read error as the I/O error it is: FFmpeg's `AVERROR(errno)`
/// back to its `errno`, anything else wrapped.
fn read_error(error: ffmpeg::Error) -> std::io::Error {
    match error {
        ffmpeg::Error::Other { errno } => std::io::Error::from_raw_os_error(errno),
        other => std::io::Error::other(other),
    }
}

/// Fingerprint the opening and the ending of `file`'s main audio track, as
/// much of each as `spec` says. `None` for a file with no audio track.
/// Blocking: run it off the async runtime.
///
/// `cancel` is read between packets: once it is set the work stops there
/// with [`FingerprintError::Cancelled`].
pub fn fingerprint_file(
    file: LibraryFile,
    spec: &FingerprintSpec,
    cancel: &AtomicBool,
) -> Result<Option<AudioFingerprint>, FingerprintError> {
    let (mut input, _metadata, path) = open_input(file)?;
    trace!(path = %path.display(), "fingerprinting audio");

    let Some(stream) = input.streams().best(ffmpeg::media::Type::Audio) else {
        debug!(path = %path.display(), "no audio track to fingerprint");
        return Ok(None);
    };
    let track = Track {
        index: stream.index(),
        time_base: stream.time_base(),
        parameters: stream.parameters(),
    };

    let duration_ms = match input.duration() {
        duration if duration > 0 => u64::try_from(duration).unwrap_or(0) / 1_000,
        _ => 0,
    };
    // Times are from the file's start, which for MPEG-TS and M2TS is seldom
    // timestamp zero.
    // SAFETY: `input` owns a live `AVFormatContext`; `start_time` is a plain
    // integer field, `AV_NOPTS_VALUE` when the demuxer found none.
    let start_us = match unsafe { (*input.as_ptr()).start_time } {
        ffmpeg::ffi::AV_NOPTS_VALUE => 0,
        start => start,
    };
    let (head_ms, tail_ms) = spec.windows(duration_ms);

    let head = decode_stretch(&mut input, &track, start_us, 0, head_ms, cancel)?;
    let tail_start_ms = duration_ms - tail_ms;
    let tail = if tail_ms > 0 {
        let target_us =
            start_us.saturating_add(i64::try_from(tail_start_ms * 1_000).unwrap_or(i64::MAX));
        // Back to the packet at or before the ending's start; a seek that
        // fails leaves the ending unheard rather than misplaced.
        match input.seek(target_us, ..target_us) {
            Ok(()) => decode_stretch(&mut input, &track, start_us, tail_start_ms, tail_ms, cancel)?,
            Err(err) => {
                debug!(path = %path.display(), %err, "could not seek to the file's ending");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    Ok(Some(AudioFingerprint {
        head: fingerprint(&head),
        tail: fingerprint(&tail),
        tail_start_ms,
    }))
}

/// The audio track being decoded.
struct Track {
    index: usize,
    time_base: ffmpeg::Rational,
    parameters: ffmpeg::codec::Parameters,
}

/// Decode `length_ms` of the track from `from_ms` into the file, the
/// demuxer already positioned at or before it: mono samples at
/// [`SAMPLE_RATE`], fewer when the file ends first.
fn decode_stretch(
    input: &mut ffmpeg::format::context::Input,
    track: &Track,
    start_us: i64,
    from_ms: u64,
    length_ms: u64,
    cancel: &AtomicBool,
) -> Result<Vec<f32>, FingerprintError> {
    let wanted = usize::try_from(length_ms * u64::from(SAMPLE_RATE) / 1_000).unwrap_or(usize::MAX);
    if wanted == 0 {
        return Ok(Vec::new());
    }
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(track.parameters.clone())?
        .decoder()
        .audio()?;
    let mut down: Option<Downsampler> = None;
    let mut frame = ffmpeg::frame::Audio::empty();
    let mut packet = ffmpeg::Packet::empty();
    let mut ended = false;

    while down.as_ref().is_none_or(|down| down.len() < wanted) && !ended {
        if cancel.load(Ordering::Relaxed) {
            return Err(FingerprintError::Cancelled);
        }
        match packet.read(input) {
            Ok(()) => {
                if packet.stream() != track.index {
                    continue;
                }
                // A packet the decoder refuses is one broken packet, not a
                // broken track.
                if decoder.send_packet(&packet).is_err() {
                    continue;
                }
            }
            Err(ffmpeg::Error::Eof) => {
                let _ = decoder.send_eof();
                ended = true;
            }
            // One corrupt packet: the demuxer resyncs past it.
            Err(ffmpeg::Error::InvalidData) => continue,
            Err(err) => return Err(FingerprintError::Read(read_error(err))),
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(mono) = mono(&frame) else {
                continue;
            };
            let at_ms = frame.timestamp().map(|pts| {
                let us = pts_us(pts, track.time_base).saturating_sub(start_us);
                u64::try_from(us).unwrap_or(0) / 1_000
            });
            match &mut down {
                Some(down) => mono.into_iter().for_each(|sample| down.push(sample)),
                None => {
                    // The first frame places the stretch: what it holds from
                    // before `from_ms` is dropped, and a gap before it is
                    // silence, so the samples keep their times.
                    let rate = u64::from(frame.rate());
                    let Some(mut started) = Downsampler::new(frame.rate()) else {
                        continue;
                    };
                    let at_ms = at_ms.unwrap_or(from_ms);
                    let skip = usize::try_from(from_ms.saturating_sub(at_ms) * rate / 1_000)
                        .unwrap_or(usize::MAX);
                    if skip >= mono.len() {
                        continue;
                    }
                    for _ in 0..at_ms.saturating_sub(from_ms) * rate / 1_000 {
                        started.push(0.0);
                    }
                    mono[skip..].iter().for_each(|sample| started.push(*sample));
                    down = Some(started);
                }
            }
        }
    }
    let mut samples = down.map(Downsampler::finish).unwrap_or_default();
    samples.truncate(wanted);
    Ok(samples)
}

/// A timestamp in `time_base` units, in microseconds.
fn pts_us(pts: i64, time_base: ffmpeg::Rational) -> i64 {
    let (num, den) = (
        i128::from(time_base.numerator()),
        i128::from(time_base.denominator()),
    );
    if den == 0 {
        return 0;
    }
    i64::try_from(i128::from(pts) * num * 1_000_000 / den).unwrap_or(0)
}

/// One sample's bytes, in the frame's sample format, as a level in [-1, 1].
type ReadSample = fn(&[u8]) -> f32;

/// `frame`'s channels averaged into one, each sample in [-1, 1]; `None` for
/// a sample format with no samples to read.
fn mono(frame: &ffmpeg::frame::Audio) -> Option<Vec<f32>> {
    use ffmpeg::format::Sample;
    use ffmpeg::format::sample::Type;

    let channels = usize::from(frame.channels()).max(1);
    let samples = frame.samples();
    let (width, planar, read): (usize, bool, ReadSample) = match frame.format() {
        Sample::U8(kind) => (1, kind == Type::Planar, |b| {
            (f32::from(b[0]) - 128.0) / 128.0
        }),
        Sample::I16(kind) => (2, kind == Type::Planar, |b| {
            f32::from(i16::from_ne_bytes([b[0], b[1]])) / 32_768.0
        }),
        Sample::I32(kind) => (4, kind == Type::Planar, |b| {
            i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
        }),
        Sample::I64(kind) => (8, kind == Type::Planar, |b| {
            i64::from_ne_bytes(b[..8].try_into().expect("eight bytes")) as f32 / 9.223_372e18
        }),
        Sample::F32(kind) => (4, kind == Type::Planar, |b| {
            f32::from_ne_bytes([b[0], b[1], b[2], b[3]])
        }),
        Sample::F64(kind) => (8, kind == Type::Planar, |b| {
            f64::from_ne_bytes(b[..8].try_into().expect("eight bytes")) as f32
        }),
        Sample::None => return None,
    };
    let mut out = vec![0.0_f32; samples];
    for channel in 0..channels {
        let (plane, first, step) = if planar {
            (channel, 0, width)
        } else {
            (0, channel * width, channels * width)
        };
        if plane >= frame.planes() {
            return None;
        }
        let data = frame.data(plane);
        for (i, sample) in out.iter_mut().enumerate() {
            let at = first + i * step;
            *sample += read(data.get(at..at + width)?);
        }
    }
    let scale = 1.0 / channels as f32;
    out.iter_mut()
        .for_each(|sample| *sample = (*sample * scale).clamp(-1.0, 1.0));
    Some(out)
}

#[cfg(test)]
#[path = "markers_media_tests.rs"]
mod markers_media_tests;
