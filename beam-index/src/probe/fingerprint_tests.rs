use proptest::prelude::*;

use super::*;

/// A deterministic noise source: xorshift64, so a test's signal is the same
/// on every run without a random-number crate.
struct Noise(u64);

impl Noise {
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    /// A sample in [-amplitude, amplitude].
    fn sample(&mut self, amplitude: f32) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32 * 2.0 - 1.0) * amplitude
    }
}

/// Noise shaped like programme audio rather than white noise: a sum of
/// tones whose pitches and loudness change every quarter second, so the
/// band energies move as music's do.
pub(crate) fn programme(seed: u64, seconds: f32) -> Vec<f32> {
    let mut noise = Noise(seed.max(1));
    let total = (seconds * SAMPLE_RATE as f32) as usize;
    let segment = SAMPLE_RATE as usize / 4;
    let mut out = Vec::with_capacity(total);
    let mut tones: Vec<(f32, f32)> = Vec::new();
    for i in 0..total {
        if i % segment == 0 {
            tones = (0..4)
                .map(|_| {
                    let hz = 300.0 + noise.next_u32() as f32 / u32::MAX as f32 * 1_700.0;
                    let gain = noise.next_u32() as f32 / u32::MAX as f32 * 0.2;
                    (hz, gain)
                })
                .collect();
        }
        let t = i as f32 / SAMPLE_RATE as f32;
        out.push(
            tones
                .iter()
                .map(|(hz, gain)| gain * (2.0 * std::f32::consts::PI * hz * t).sin())
                .sum(),
        );
    }
    out
}

/// `parts`, one after another.
fn joined(parts: &[&[f32]]) -> Vec<f32> {
    parts.iter().flat_map(|part| part.iter().copied()).collect()
}

#[test]
fn the_fft_is_the_discrete_fourier_transform() {
    let mut noise = Noise(7);
    let signal: Vec<f64> = (0..64).map(|_| f64::from(noise.sample(1.0))).collect();
    let mut re = signal.clone();
    let mut im = vec![0.0; 64];
    fft(&mut re, &mut im);
    for k in 0..64 {
        let (mut dr, mut di) = (0.0, 0.0);
        for (n, x) in signal.iter().enumerate() {
            let angle = -2.0 * std::f64::consts::PI * (k * n) as f64 / 64.0;
            dr += x * angle.cos();
            di += x * angle.sin();
        }
        assert!(
            (re[k] - dr).abs() < 1e-9 && (im[k] - di).abs() < 1e-9,
            "bin {k}"
        );
    }
}

#[test]
fn the_downsampler_keeps_time_at_any_rate() {
    for rate in [6_000_u32, 8_000, 11_025, 44_100, 48_000, 96_000] {
        let mut down = Downsampler::new(rate).unwrap();
        let seconds = 3;
        for _ in 0..rate * seconds {
            down.push(0.5);
        }
        let out = down.finish();
        // Audio slower than the output rate ends part-way into its last
        // output period, which is not made.
        assert!(
            out.len().abs_diff((SAMPLE_RATE * seconds) as usize) <= 1,
            "{rate} Hz: {}",
            out.len()
        );
        assert!(
            out.iter().all(|sample| (sample - 0.5).abs() < 1e-6),
            "{rate} Hz"
        );
    }
    assert!(Downsampler::new(0).is_none());
}

#[test]
fn the_downsampler_averages_each_output_period() {
    let mut down = Downsampler::new(16_000).unwrap();
    for sample in [1.0, 0.0, 0.5, 0.5, -1.0, -1.0] {
        down.push(sample);
    }
    assert_eq!(down.finish(), vec![0.5, 0.5, -1.0]);
}

#[test]
fn silence_gives_no_bits_and_each_window_one_entry() {
    let silence = vec![0.0_f32; WINDOW + 9 * HOP];
    let prints = fingerprint(&silence);
    assert_eq!(prints.len(), 10);
    assert!(prints.iter().all(Option::is_none));

    let sound = programme(3, 5.0);
    let prints = fingerprint(&sound);
    assert_eq!(prints.len(), (sound.len() - WINDOW) / HOP + 1);
    assert_eq!(prints[0], None, "the first window has none before it");
    assert!(prints[1..].iter().all(Option::is_some));

    assert!(fingerprint(&sound[..WINDOW - 1]).is_empty());
}

/// Two episodes sharing thirty seconds of a theme at different places --
/// each copy with its own faint noise and at its own level, as two
/// encodings of one theme are -- share that span and nothing else.
#[test]
fn two_episodes_share_the_span_they_have_in_common() {
    let theme = programme(11, 30.0);
    let mut hiss = Noise(5);
    let copy = |gain: f32, hiss: &mut Noise| -> Vec<f32> {
        theme
            .iter()
            .map(|s| s * gain + hiss.sample(0.002))
            .collect()
    };
    let a = joined(&[
        &programme(21, 20.0),
        &copy(1.0, &mut hiss),
        &programme(22, 40.0),
    ]);
    let b = joined(&[
        &programme(31, 50.0),
        &copy(0.7, &mut hiss),
        &programme(32, 10.0),
    ]);

    let span = longest_shared_span(
        &fingerprint(&a),
        &fingerprint(&b),
        15_000 / FRAME_MS as usize,
    )
    .expect("the theme is shared");

    let seconds = |frames: &Range<usize>| {
        let (start, end) = span_ms(frames);
        (start as f64 / 1_000.0, end as f64 / 1_000.0)
    };
    let (a_start, a_end) = seconds(&span.a);
    let (b_start, b_end) = seconds(&span.b);
    assert!(
        (a_start - 20.0).abs() < 0.5 && (a_end - 50.0).abs() < 0.5,
        "{a_start}..{a_end}"
    );
    assert!(
        (b_start - 50.0).abs() < 0.5 && (b_end - 80.0).abs() < 0.5,
        "{b_start}..{b_end}"
    );

    let unrelated = programme(41, 90.0);
    assert_eq!(
        longest_shared_span(
            &fingerprint(&a),
            &fingerprint(&unrelated),
            15_000 / FRAME_MS as usize
        ),
        None
    );
}

/// A fingerprint of random windows with `shared` copied in at `at`.
fn with_shared(seed: u64, len: usize, shared: &[SubFingerprint], at: usize) -> Vec<SubFingerprint> {
    let mut noise = Noise(seed);
    let mut prints: Vec<SubFingerprint> = (0..len).map(|_| Some(noise.next_u32())).collect();
    prints[at..at + shared.len()].copy_from_slice(shared);
    prints
}

fn frames(ms: u64) -> usize {
    (ms / FRAME_MS) as usize
}

#[test]
fn an_episode_is_given_the_intro_and_credits_it_shares_with_its_season() {
    let mut noise = Noise(99);
    let theme: Vec<SubFingerprint> = (0..frames(40_000))
        .map(|_| Some(noise.next_u32()))
        .collect();
    let credits: Vec<SubFingerprint> = (0..frames(60_000))
        .map(|_| Some(noise.next_u32()))
        .collect();
    let episode = AudioFingerprint {
        head: with_shared(1, frames(300_000), &theme, frames(90_000)),
        tail: with_shared(2, frames(240_000), &credits, frames(150_000)),
        tail_start_ms: 1_200_000,
    };
    let other = AudioFingerprint {
        head: with_shared(3, frames(300_000), &theme, frames(10_000)),
        tail: with_shared(4, frames(240_000), &credits, frames(100_000)),
        tail_start_ms: 1_100_000,
    };
    let stranger = AudioFingerprint {
        head: with_shared(5, frames(300_000), &[], 0),
        tail: with_shared(6, frames(240_000), &[], 0),
        tail_start_ms: 900_000,
    };

    let markers = detect_markers(&episode, &[&stranger, &other]);

    // A span is timed from its windows' middles, half a window after the
    // frame index that starts each.
    let near = |got: u64, want: u64| got.abs_diff(want) <= WINDOW_MS / 2;
    assert_eq!(markers.len(), 2, "{markers:?}");
    let (intro, end_credits) = (markers[0], markers[1]);
    assert_eq!(intro.kind, MarkerKind::Intro);
    assert!(
        near(intro.start_ms, 90_000) && near(intro.end_ms, 130_000),
        "{intro:?}"
    );
    assert_eq!(end_credits.kind, MarkerKind::Credits);
    assert!(
        near(end_credits.start_ms, 1_350_000) && near(end_credits.end_ms, 1_410_000),
        "{end_credits:?}"
    );

    assert!(detect_markers(&episode, &[&stranger]).is_empty());
    assert!(detect_markers(&episode, &[]).is_empty());
}

#[test]
fn a_shared_span_too_short_or_too_long_marks_nothing() {
    let mut noise = Noise(42);
    let mut shared = |ms: u64| -> Vec<SubFingerprint> {
        (0..frames(ms)).map(|_| Some(noise.next_u32())).collect()
    };
    let short = shared(10_000);
    let long = shared(200_000);
    for theme in [short, long] {
        let episode = AudioFingerprint {
            head: with_shared(7, frames(300_000), &theme, 0),
            ..AudioFingerprint::default()
        };
        let other = AudioFingerprint {
            head: with_shared(8, frames(300_000), &theme, frames(50_000)),
            ..AudioFingerprint::default()
        };
        assert!(
            detect_markers(&episode, &[&other]).is_empty(),
            "{} frames",
            theme.len()
        );
    }
}

#[test]
fn a_file_is_heard_for_at_most_a_third_from_its_start_and_a_quarter_from_its_end() {
    let spec = FingerprintSpec::default();
    assert_eq!(spec.windows(22 * 60 * 1_000), (440_000, 300_000));
    assert_eq!(spec.windows(60 * 60 * 1_000), (600_000, 300_000));
    assert_eq!(spec.windows(0), (600_000, 0));
}

proptest! {
    /// Whatever the two fingerprints, a shared span lies inside both, is as
    /// long in each, is at least as long as asked, and starts and ends on
    /// windows that are the same sound.
    #[test]
    fn a_shared_span_is_a_matching_stretch_of_both(
        a in proptest::collection::vec(proptest::option::of(0_u32..4), 0..60),
        b in proptest::collection::vec(proptest::option::of(0_u32..4), 0..60),
        min_frames in 1_usize..10,
    ) {
        if let Some(span) = longest_shared_span(&a, &b, min_frames) {
            prop_assert!(span.a.end <= a.len() && span.b.end <= b.len());
            prop_assert_eq!(span.a.len(), span.b.len());
            prop_assert!(span.a.len() >= min_frames);
            prop_assert!(same(a[span.a.start], b[span.b.start]));
            prop_assert!(same(a[span.a.end - 1], b[span.b.end - 1]));
        }
    }

    /// Fingerprinting never panics, and gives one entry per window.
    #[test]
    fn any_audio_fingerprints_one_entry_per_window(
        samples in proptest::collection::vec(-1.0_f32..1.0, 0..(WINDOW * 3)),
    ) {
        let prints = fingerprint(&samples);
        let windows = if samples.len() < WINDOW { 0 } else { (samples.len() - WINDOW) / HOP + 1 };
        prop_assert_eq!(prints.len(), windows);
    }
}
