//! Audio fingerprints, and the spans two episodes share (issue #332).
//!
//! A season's episodes open on the same theme and close on the same credits
//! music, while the rest of their audio differs. Beam finds those spans by
//! fingerprinting the start and the end of each episode's audio and looking
//! for the longest stretch two episodes have in common.
//!
//! The fingerprint is Haitsma and Kalker's: the audio, mono at
//! [`SAMPLE_RATE`], is cut into overlapping windows [`FRAME_MS`] apart, and
//! each window gives 32 bits -- whether the energy difference between two
//! neighbouring frequency bands, between 300 Hz and 2 kHz, rose or fell
//! since the window before. The bits survive re-encoding, so two copies of
//! one theme agree on most of them, and two different sounds agree on half.
//! A window too quiet to say anything gives no bits: two silences are not a
//! shared intro.
//!
//! Everything here is pure, so it is tested on synthesised signals; the
//! FFmpeg side that decodes a file's audio is [`super::markers`].

use std::ops::Range;

use beam_domain::models::chapter::{MarkerKind, PlaybackMarker};

/// The rate, in samples per second, the audio is fingerprinted at.
pub const SAMPLE_RATE: u32 = 8_000;

/// Samples in one window: 256 ms.
const WINDOW: usize = 2_048;

/// Samples between two windows' starts.
const HOP: usize = 512;

/// The time between two windows' starts, in milliseconds.
pub const FRAME_MS: u64 = (HOP as u64 * 1_000) / SAMPLE_RATE as u64;

/// A window's length, in milliseconds.
const WINDOW_MS: u64 = (WINDOW as u64 * 1_000) / SAMPLE_RATE as u64;

/// The frequency range the bands cover, in hertz.
const LOWEST_HZ: f64 = 300.0;
const HIGHEST_HZ: f64 = 2_000.0;

/// Bands: one more than the bits, since each bit compares two neighbours.
const BANDS: usize = 33;

/// A window whose root-mean-square level is below this -- about -60 dBFS --
/// is silence, and gives no bits.
const SILENCE_RMS: f32 = 1e-3;

/// The most bits two windows may differ in and still be the same sound.
/// Two unrelated windows differ in 16 on average, with a standard deviation
/// of under 3, so this is nearly three clear of chance -- far enough that
/// chance matches, which cluster while a sound holds, seldom bridge a gap
/// into a run.
const MAX_BIT_ERRORS: u32 = 8;

/// The most windows in a row that may fail to match inside one shared span:
/// a moment of dialogue over a theme, or a glitch in one copy.
const MAX_GAP_FRAMES: usize = 8;

/// One window's bits; `None` for a window of silence, or the first window,
/// which has no window before it to compare with.
pub type SubFingerprint = Option<u32>;

/// What [`fingerprint`] makes of one file's audio: its opening and its
/// ending, each a window every [`FRAME_MS`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioFingerprint {
    /// The opening, its first window starting at the file's start.
    pub head: Vec<SubFingerprint>,
    /// The ending, its first window starting `tail_start_ms` into the file;
    /// empty when the file is too short to have an ending apart from its
    /// opening.
    pub tail: Vec<SubFingerprint>,
    /// Where `tail` starts, in milliseconds from the file's start.
    pub tail_start_ms: u64,
}

/// Which part of a file to fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FingerprintSpec {
    /// The most of the file's opening to hear: an intro starts within it.
    pub head_max_ms: u64,
    /// The most of the file's ending to hear: the credits start within it.
    pub tail_max_ms: u64,
}

impl Default for FingerprintSpec {
    /// The first ten minutes, and the last five.
    fn default() -> Self {
        Self {
            head_max_ms: 10 * 60 * 1_000,
            tail_max_ms: 5 * 60 * 1_000,
        }
    }
}

impl FingerprintSpec {
    /// How much of a file `duration_ms` long to hear at its opening and at
    /// its ending: at most a third of it from the start and a quarter from
    /// the end, so the two never overlap. A file of unknown length (`0`) is
    /// heard from the start only.
    pub fn windows(&self, duration_ms: u64) -> (u64, u64) {
        if duration_ms == 0 {
            return (self.head_max_ms, 0);
        }
        (
            self.head_max_ms.min(duration_ms / 3),
            self.tail_max_ms.min(duration_ms / 4),
        )
    }
}

/// Audio at any rate, brought to mono at [`SAMPLE_RATE`] by averaging the
/// samples that fall in each output sample's period. Fed as it is decoded.
#[derive(Debug)]
pub struct Downsampler {
    rate: u64,
    consumed: u64,
    bin: u64,
    sum: f32,
    count: u32,
    last: f32,
    out: Vec<f32>,
}

impl Downsampler {
    /// One for audio at `rate` samples per second; `None` for a rate of 0.
    pub fn new(rate: u32) -> Option<Self> {
        (rate > 0).then(|| Self {
            rate: u64::from(rate),
            consumed: 0,
            bin: 0,
            sum: 0.0,
            count: 0,
            last: 0.0,
            out: Vec::new(),
        })
    }

    /// Take one mono sample.
    pub fn push(&mut self, sample: f32) {
        let bin = self.consumed * u64::from(SAMPLE_RATE) / self.rate;
        self.consumed += 1;
        if bin != self.bin {
            self.flush_bin();
            // Audio slower than the output rate skips output samples: each
            // holds the last one, so time keeps its place.
            while self.bin + 1 < bin {
                self.out.push(self.last);
                self.bin += 1;
            }
            self.bin = bin;
        }
        self.sum += sample;
        self.count += 1;
    }

    fn flush_bin(&mut self) {
        if self.count > 0 {
            self.last = self.sum / self.count as f32;
            self.out.push(self.last);
        }
        self.sum = 0.0;
        self.count = 0;
    }

    /// The samples made so far.
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Whether none have been made.
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// Every sample, the last part-filled one included.
    pub fn finish(mut self) -> Vec<f32> {
        self.flush_bin();
        self.out
    }
}

/// An in-place radix-2 fast Fourier transform of `re` and `im`, whose
/// length is a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sin, cos) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (br, bi) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                re[b] = re[a] - br;
                im[b] = im[a] - bi;
                re[a] += br;
                im[a] += bi;
            }
        }
        len <<= 1;
    }
}

/// The spectrum bins each band sums, logarithmically spaced.
fn band_bins() -> [Range<usize>; BANDS] {
    let bin_hz = f64::from(SAMPLE_RATE) / WINDOW as f64;
    let edge = |band: usize| {
        let hz = LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ).powf(band as f64 / BANDS as f64);
        (hz / bin_hz).round() as usize
    };
    std::array::from_fn(|band| edge(band)..edge(band + 1).max(edge(band) + 1))
}

/// The fingerprint of `samples`, mono at [`SAMPLE_RATE`]: one entry per
/// window, [`FRAME_MS`] apart, the first starting at the first sample.
pub fn fingerprint(samples: &[f32]) -> Vec<SubFingerprint> {
    if samples.len() < WINDOW {
        return Vec::new();
    }
    let bands = band_bins();
    let hann: Vec<f64> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / WINDOW as f64).cos())
        .collect();
    let mut re = vec![0.0; WINDOW];
    let mut im = vec![0.0; WINDOW];
    let mut previous: Option<[f64; BANDS]> = None;
    let mut prints = Vec::with_capacity((samples.len() - WINDOW) / HOP + 1);
    for start in (0..=samples.len() - WINDOW).step_by(HOP) {
        let window = &samples[start..start + WINDOW];
        let power: f64 = window.iter().map(|s| f64::from(*s).powi(2)).sum();
        if (power / WINDOW as f64).sqrt() < f64::from(SILENCE_RMS) {
            previous = None;
            prints.push(None);
            continue;
        }
        for (i, sample) in window.iter().enumerate() {
            re[i] = f64::from(*sample) * hann[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let energy: [f64; BANDS] = std::array::from_fn(|band| {
            bands[band]
                .clone()
                .map(|bin| re[bin] * re[bin] + im[bin] * im[bin])
                .sum()
        });
        prints.push(previous.map(|before| {
            (0..BANDS - 1).fold(0_u32, |bits, m| {
                let now = energy[m] - energy[m + 1];
                let then = before[m] - before[m + 1];
                bits | (u32::from(now - then > 0.0) << m)
            })
        }));
        previous = Some(energy);
    }
    prints
}

/// Whether two windows are the same sound.
fn same(a: SubFingerprint, b: SubFingerprint) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a ^ b).count_ones() <= MAX_BIT_ERRORS,
        _ => false,
    }
}

/// The longest stretch two fingerprints share: the windows of each, as
/// ranges of the same length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedSpan {
    pub a: Range<usize>,
    pub b: Range<usize>,
}

/// Windows in a row that must match where a shared span starts and ends: a
/// lone chance match just outside the span, a gap away, is not its edge.
const EDGE_FRAMES: usize = 3;

/// `run` -- windows of `a`, beside `b`'s `shift` further on -- with each end
/// moved inwards to the first [`EDGE_FRAMES`] windows in a row that match;
/// `None` when no such windows are left.
fn trimmed(
    a: &[SubFingerprint],
    b: &[SubFingerprint],
    shift: isize,
    run: Range<usize>,
) -> Option<(usize, usize)> {
    let matches = |i: usize| {
        i.checked_add_signed(shift)
            .and_then(|j| b.get(j))
            .is_some_and(|other| same(a[i], *other))
    };
    let solid = |i: usize| (i..i + EDGE_FRAMES).all(|k| k < run.end && matches(k));
    let start = run.clone().find(|i| solid(*i))?;
    let end = (start..run.end)
        .rev()
        .find(|i| *i + 1 >= start + EDGE_FRAMES && solid(*i + 1 - EDGE_FRAMES))?
        + 1;
    Some((start, end))
}

/// The longest stretch of at least `min_frames` windows `a` and `b` share,
/// each at its own place: windows that are the same sound, with at most
/// [`MAX_GAP_FRAMES`] in a row that are not. `None` when none is that long.
pub fn longest_shared_span(
    a: &[SubFingerprint],
    b: &[SubFingerprint],
    min_frames: usize,
) -> Option<SharedSpan> {
    let mut best: Option<SharedSpan> = None;
    let mut consider = |start: usize, end: usize, shift: isize| {
        let Some((start, end)) = trimmed(a, b, shift, start..end) else {
            return;
        };
        let length = end - start;
        if length >= min_frames.max(1) && best.as_ref().is_none_or(|best| length > best.a.len()) {
            let b_start = start.checked_add_signed(shift).expect("on the diagonal");
            best = Some(SharedSpan {
                a: start..end,
                b: b_start..b_start + length,
            });
        }
    };
    // Each diagonal is one alignment of `b` against `a`: `b[i + shift]`
    // beside `a[i]`.
    let shifts = -(a.len() as isize - 1)..(b.len() as isize);
    for shift in shifts {
        let first = usize::try_from(-shift).unwrap_or(0);
        let last = a
            .len()
            .min(usize::try_from(b.len() as isize - shift).unwrap_or(0));
        // The run being followed: where it started, and one past the last
        // window that matched.
        let mut run: Option<(usize, usize)> = None;
        for (i, own) in a.iter().enumerate().take(last).skip(first) {
            let j = i.checked_add_signed(shift).expect("on the diagonal");
            if same(*own, b[j]) {
                run = Some(match run {
                    Some((start, _)) => (start, i + 1),
                    None => (i, i + 1),
                });
            } else if let Some((start, end)) = run
                && i - end >= MAX_GAP_FRAMES
            {
                consider(start, end, shift);
                run = None;
            }
        }
        if let Some((start, end)) = run {
            consider(start, end, shift);
        }
    }
    best
}

/// How long a span must be to be an intro or the credits, and how long it
/// may be.
const MIN_SPAN_MS: u64 = 15_000;
const MAX_INTRO_MS: u64 = 150_000;
const MAX_CREDITS_MS: u64 = 240_000;

/// The time, in milliseconds from a fingerprint's start, that its windows
/// `frames` cover: from the middle of the first to the middle of the last,
/// widened by half a frame each way.
fn span_ms(frames: &Range<usize>) -> (u64, u64) {
    let centre = |frame: usize| frame as u64 * FRAME_MS + WINDOW_MS / 2;
    (
        centre(frames.start).saturating_sub(FRAME_MS / 2),
        centre(frames.end - 1) + FRAME_MS / 2,
    )
}

/// The longest span of `kind` that `own` -- one part of a file's
/// fingerprint, starting `offset_ms` into it -- shares with any of
/// `others`, as a marker on the file's timeline, if one lasts between
/// [`MIN_SPAN_MS`] and `max_ms`.
fn shared_marker<'a>(
    kind: MarkerKind,
    own: &[SubFingerprint],
    offset_ms: u64,
    others: impl Iterator<Item = &'a [SubFingerprint]>,
    max_ms: u64,
) -> Option<PlaybackMarker> {
    let min_frames = MIN_SPAN_MS.div_ceil(FRAME_MS) as usize;
    others
        .filter_map(|other| longest_shared_span(own, other, min_frames))
        .map(|span| span_ms(&span.a))
        .filter(|(start, end)| end - start <= max_ms)
        .max_by_key(|(start, end)| (end - start, std::cmp::Reverse(*start)))
        .map(|(start, end)| PlaybackMarker {
            kind,
            start_ms: offset_ms + start,
            end_ms: offset_ms + end,
        })
}

/// The intro and credits of the episode fingerprinted as `episode`, from
/// what it shares with `others` -- other episodes of its season: the longest
/// span of its opening that one of them also has is its intro, and the
/// longest of its ending its credits. In the order they start; none when it
/// shares nothing long enough.
pub fn detect_markers(
    episode: &AudioFingerprint,
    others: &[&AudioFingerprint],
) -> Vec<PlaybackMarker> {
    let intro = shared_marker(
        MarkerKind::Intro,
        &episode.head,
        0,
        others.iter().map(|other| other.head.as_slice()),
        MAX_INTRO_MS,
    );
    let credits = shared_marker(
        MarkerKind::Credits,
        &episode.tail,
        episode.tail_start_ms,
        others.iter().map(|other| other.tail.as_slice()),
        MAX_CREDITS_MS,
    );
    let mut markers: Vec<PlaybackMarker> = intro.into_iter().chain(credits).collect();
    markers.sort_by_key(|marker| (marker.start_ms, marker.end_ms));
    markers
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
pub(crate) mod fingerprint_tests;
