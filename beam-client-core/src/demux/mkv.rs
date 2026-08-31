//! A Matroska/WebM extractor over a foreign byte source.

use crate::capability::{AudioCodec, VideoCodec};
use crate::demux::reader::{ByteSourceReader, FaultLog};
use crate::error::ExtractorError;
use crate::ports::byte_source::ByteSource;
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Nanoseconds in a second, as the Matroska timestamp scale expresses it.
const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

/// How far back a seek will search for a keyframe before giving up, in
/// seconds. Beyond this a file is either keyframe-starved or not really
/// seekable, and scanning further costs more than starting late does.
const SEEK_BACKOFF_SECONDS: [f64; 7] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// How many frames a single keyframe search will examine, so a pathological
/// file cannot turn a seek into an unbounded scan.
const SEEK_SCAN_FRAME_LIMIT: usize = 4096;

/// What a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TrackKind {
    /// Video.
    Video,
    /// Audio.
    Audio,
    /// Subtitles, of any format.
    Subtitle,
    /// Anything else the container may carry.
    Other,
}

/// How a subtitle track is encoded.
///
/// Reported even for formats the platform cannot render, because under direct
/// play that is a permanent property of the file the viewer may need to act on
/// -- the same reasoning `capability::select` applies to undecodable video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SubtitleFormat {
    /// SubRip, plain text.
    SubRip,
    /// Advanced SubStation Alpha, styled text.
    Ass,
    /// WebVTT, plain text.
    WebVtt,
    /// Blu-ray presentation graphics: bitmaps, not text.
    Pgs,
    /// DVD subtitles: bitmaps, not text.
    VobSub,
    /// Present, but in a format this extractor does not name.
    Unknown,
}

/// One track inside the container.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ExtractorTrack {
    /// The container's own track number, and the identifier every other method
    /// here takes. Not an index: Matroska track numbers are one-based and need
    /// not be contiguous.
    pub number: u64,
    /// What the track carries.
    pub kind: TrackKind,
    /// The container's raw `CodecID`, kept verbatim for diagnostics.
    pub codec_id: String,
    /// The normalised video codec, for a video track.
    pub video_codec: Option<VideoCodec>,
    /// The normalised audio codec, for an audio track.
    pub audio_codec: Option<AudioCodec>,
    /// The subtitle format, for a subtitle track.
    pub subtitle_format: Option<SubtitleFormat>,
    /// `CodecPrivate` verbatim -- `avcC` for H.264, `hvcC` for HEVC, `av1C`
    /// for AV1, the magic cookie for AAC. The platform needs these bytes to
    /// build a format description, and this extractor deliberately does not
    /// interpret them: parsing them here would duplicate what CoreMedia
    /// already does, and get it wrong differently.
    pub codec_private: Vec<u8>,
    /// Coded width in pixels, for a video track.
    pub width: u32,
    /// Coded height in pixels, for a video track.
    pub height: u32,
    /// Sampling frequency in hertz, for an audio track.
    pub sample_rate: u32,
    /// Channel count, for an audio track.
    pub channels: u16,
    /// The track's language, as the container spells it.
    pub language: Option<String>,
    /// The track's human-readable name, where it has one.
    pub name: Option<String>,
    /// Whether the container marks this track as the default for its kind.
    pub is_default: bool,
    /// Whether the container marks this track as forced.
    pub is_forced: bool,
}

/// One encoded, undecoded sample.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct EncodedSample {
    /// The track this sample belongs to.
    pub track: u64,
    /// The encoded bytes, exactly as the container stored them.
    pub data: Vec<u8>,
    /// Presentation timestamp, in seconds from the start of the file.
    pub pts_seconds: f64,
    /// The sample's own duration, where the container states one. Absent for
    /// most video, present for subtitles.
    pub duration_seconds: Option<f64>,
    /// Whether a decoder can start here.
    pub is_keyframe: bool,
}

/// A Matroska or WebM file, read through a platform-supplied byte source.
///
/// Every method takes `&self` and locks internally, because UniFFI exposes an
/// object by shared reference. The lock is not contention-sensitive: one
/// playback session drives one extractor from one thread.
#[derive(Debug, uniffi::Object)]
pub struct MatroskaExtractor {
    inner: Mutex<Inner>,
    tracks: Vec<ExtractorTrack>,
    /// Nanoseconds per timestamp tick.
    timestamp_scale: u64,
    duration_seconds: Option<f64>,
    fault: FaultLog,
}

#[derive(Debug)]
struct Inner {
    file: MatroskaFile<ByteSourceReader>,
    selected: HashSet<u64>,
}

#[uniffi::export]
impl MatroskaExtractor {
    /// Open `source` as a Matroska or WebM file.
    ///
    /// Reads only the header and track entries; no media bytes are fetched
    /// until [`MatroskaExtractor::next_sample`] is called.
    #[uniffi::constructor]
    pub fn open(source: Arc<dyn ByteSource>) -> Result<Arc<Self>, ExtractorError> {
        let reader = ByteSourceReader::new(source);
        // Taken before the reader disappears into the parser, which offers no
        // way back to it.
        let fault = reader.fault_log();
        let file = MatroskaFile::open(reader).map_err(|error| map_demux_error(error, &fault))?;

        let timestamp_scale = file.info().timestamp_scale().get();
        let duration_seconds = file
            .info()
            .duration()
            .map(|ticks| ticks * timestamp_scale as f64 / NANOS_PER_SECOND);

        let tracks: Vec<ExtractorTrack> = file.tracks().iter().map(describe_track).collect();

        // Selecting everything by default means a caller that only wants to
        // enumerate tracks does not have to make a second call, and a caller
        // that wants a subset says so explicitly.
        let selected = tracks.iter().map(|track| track.number).collect();

        Ok(Arc::new(Self {
            inner: Mutex::new(Inner { file, selected }),
            tracks,
            timestamp_scale,
            duration_seconds,
            fault,
        }))
    }

    /// Every track the container declares, in the order it declares them.
    #[must_use]
    pub fn tracks(&self) -> Vec<ExtractorTrack> {
        self.tracks.clone()
    }

    /// The file's duration in seconds, where the container states one.
    #[must_use]
    pub fn duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
    }

    /// Restrict [`MatroskaExtractor::next_sample`] to these track numbers.
    ///
    /// An empty list selects nothing, which is a valid -- if useless -- state
    /// rather than a shorthand for "everything": a caller that computed an
    /// empty selection has a bug, and silently playing all tracks would hide
    /// it behind a wall of audio.
    pub fn select_tracks(&self, tracks: Vec<u64>) {
        let mut inner = self.lock();
        inner.selected = tracks.into_iter().collect();
    }

    /// The next sample on a selected track, or `None` at end of file.
    pub fn next_sample(&self) -> Result<Option<EncodedSample>, ExtractorError> {
        let mut inner = self.lock();
        let scale = self.timestamp_scale;

        let mut frame = Frame::default();
        loop {
            if !inner
                .file
                .next_frame(&mut frame)
                .map_err(|error| map_demux_error(error, &self.fault))?
            {
                return Ok(None);
            }
            if inner.selected.contains(&frame.track) {
                return Ok(Some(sample_from_frame(&frame, scale)));
            }
        }
    }

    /// Position the extractor at the last keyframe at or before `seconds`, and
    /// report where it actually landed.
    ///
    /// Seeking to the requested position directly would be wrong: Matroska
    /// positions to the first frame at or after a timestamp, and that frame is
    /// usually not a keyframe, so a decoder handed it would produce nothing
    /// until the next one arrived. Landing on the preceding keyframe lets the
    /// caller decode forward and discard, which is what every player does.
    ///
    /// The returned position is the caller's, not a suggestion: it is the
    /// timestamp of the first sample `next_sample` will now return, so the
    /// caller can decide how much to discard.
    pub fn seek(&self, seconds: f64) -> Result<f64, ExtractorError> {
        let target = seconds.max(0.0);
        let target_ticks = self.ticks_for(target);
        let mut inner = self.lock();

        // With no video track selected there is no keyframe to find, and audio
        // frames are all independently decodable -- so the requested position
        // is reachable exactly.
        if !inner.has_selected_video(&self.tracks) {
            seek_ticks(&mut inner, target_ticks, target)?;
            return Ok(target);
        }

        // Selected video tracks only, matching `has_selected_video` above. A
        // file with more than one video track would otherwise let the scan
        // land on a keyframe belonging to a track `next_sample` filters out,
        // leaving the reader mid-GOP for the track actually being played --
        // which is a corrupt picture after a seek rather than an error.
        let video_tracks: HashSet<u64> = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .map(|track| track.number)
            .filter(|number| inner.selected.contains(number))
            .collect();

        for backoff in SEEK_BACKOFF_SECONDS {
            let from_ticks = target_ticks.saturating_sub(self.ticks_for(backoff));
            seek_ticks(&mut inner, from_ticks, target)?;

            let mut best: Option<u64> = None;
            let mut frame = Frame::default();
            for _ in 0..SEEK_SCAN_FRAME_LIMIT {
                if !inner
                    .file
                    .next_frame(&mut frame)
                    .map_err(|error| map_demux_error(error, &self.fault))?
                {
                    break;
                }
                if frame.timestamp > target_ticks {
                    break;
                }
                if video_tracks.contains(&frame.track) && is_keyframe(&frame) {
                    best = Some(frame.timestamp);
                }
            }

            if let Some(keyframe_ticks) = best {
                seek_ticks(&mut inner, keyframe_ticks, target)?;
                return Ok(self.seconds_for(keyframe_ticks));
            }

            if from_ticks == 0 {
                break;
            }
        }

        // No keyframe at or before the target. Landing on the target itself is
        // better than refusing: the decoder will drop frames until it finds
        // one it can start from, which is a late picture rather than no
        // playback at all.
        seek_ticks(&mut inner, target_ticks, target)?;
        Ok(target)
    }
}

impl MatroskaExtractor {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a previous call panicked mid-parse. Recovering
        // the guard is correct here: the parser holds no invariant that a
        // panic could have left half-applied that a subsequent call would not
        // simply fail on again.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ticks_for(&self, seconds: f64) -> u64 {
        let ticks = seconds * NANOS_PER_SECOND / self.timestamp_scale as f64;
        if ticks.is_finite() && ticks > 0.0 {
            ticks as u64
        } else {
            0
        }
    }

    fn seconds_for(&self, ticks: u64) -> f64 {
        ticks as f64 * self.timestamp_scale as f64 / NANOS_PER_SECOND
    }
}

impl Inner {
    fn has_selected_video(&self, tracks: &[ExtractorTrack]) -> bool {
        tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video && self.selected.contains(&track.number))
    }
}

/// Whether a decoder can start at this frame.
///
/// `is_keyframe` is only populated for SimpleBlocks; a frame carried in a
/// BlockGroup reports `None`. Treating that as a keyframe is the lesser error:
/// the alternative is a seek that finds no start point at all and never
/// renders, where this at worst shows one corrupt picture before the decoder
/// resynchronises.
fn is_keyframe(frame: &Frame) -> bool {
    frame.is_keyframe.unwrap_or(true)
}

fn sample_from_frame(frame: &Frame, timestamp_scale: u64) -> EncodedSample {
    let scale = timestamp_scale as f64 / NANOS_PER_SECOND;
    EncodedSample {
        track: frame.track,
        data: frame.data.clone(),
        pts_seconds: frame.timestamp as f64 * scale,
        duration_seconds: frame.duration.map(|ticks| ticks as f64 * scale),
        is_keyframe: is_keyframe(frame),
    }
}

fn seek_ticks(inner: &mut Inner, ticks: u64, requested: f64) -> Result<(), ExtractorError> {
    inner
        .file
        .seek(ticks)
        .map_err(|error| ExtractorError::Seek {
            seconds: requested,
            detail: error.to_string(),
        })
}

fn describe_track(track: &matroska_demuxer::TrackEntry) -> ExtractorTrack {
    let codec_id = track.codec_id().to_owned();
    let kind = match track.track_type() {
        TrackType::Video => TrackKind::Video,
        TrackType::Audio => TrackKind::Audio,
        TrackType::Subtitle => TrackKind::Subtitle,
        _ => TrackKind::Other,
    };

    let video = track.video();
    let audio = track.audio();

    ExtractorTrack {
        number: track.track_number().get(),
        kind,
        video_codec: (kind == TrackKind::Video)
            .then(|| VideoCodec::from_matroska_codec_id(&codec_id)),
        audio_codec: (kind == TrackKind::Audio)
            .then(|| AudioCodec::from_matroska_codec_id(&codec_id)),
        subtitle_format: (kind == TrackKind::Subtitle).then(|| subtitle_format(&codec_id)),
        codec_private: track.codec_private().unwrap_or_default().to_vec(),
        width: video.map_or(0, |video| clamp_u32(video.pixel_width().get())),
        height: video.map_or(0, |video| clamp_u32(video.pixel_height().get())),
        sample_rate: audio.map_or(0, |audio| clamp_u32(audio.sampling_frequency() as u64)),
        channels: audio.map_or(0, |audio| {
            u16::try_from(audio.channels().get()).unwrap_or(u16::MAX)
        }),
        language: track.language().map(str::to_owned),
        name: track.name().map(str::to_owned),
        is_default: track.flag_default(),
        is_forced: track.flag_forced(),
        codec_id,
    }
}

fn subtitle_format(codec_id: &str) -> SubtitleFormat {
    match codec_id.trim().to_ascii_uppercase().as_str() {
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => SubtitleFormat::SubRip,
        "S_TEXT/ASS" | "S_TEXT/SSA" => SubtitleFormat::Ass,
        "S_TEXT/WEBVTT" => SubtitleFormat::WebVtt,
        "S_HDMV/PGS" => SubtitleFormat::Pgs,
        "S_VOBSUB" => SubtitleFormat::VobSub,
        _ => SubtitleFormat::Unknown,
    }
}

fn clamp_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The containers this extractor can open, lowercased.
///
/// Exported so the platform builds its `DeviceProfile` from what the client can
/// actually demux rather than from a list hand-copied into Swift. That copy is
/// exactly how a device ends up claiming to support a container the extractor
/// was never taught, and `capability::select` would then offer a source that
/// cannot play.
#[uniffi::export]
#[must_use]
pub fn probe_containers() -> Vec<String> {
    vec!["mkv".to_owned(), "webm".to_owned(), "mka".to_owned()]
}

/// Classify a parser failure, consulting the reader's fault log first.
///
/// An `IoError` here is ambiguous on its own: it is what the parser reports
/// both when the byte source failed *and* when a corrupt file declared an
/// element longer than the bytes that follow it. Only the reader knows which
/// happened, so it is asked rather than guessed at -- the difference is
/// "check your connection" against "this file is damaged", and getting it
/// backwards sends the viewer to retry something that will never work.
fn map_demux_error(error: matroska_demuxer::DemuxError, fault: &FaultLog) -> ExtractorError {
    use matroska_demuxer::DemuxError;

    if let Some(source_error) = fault
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return source_error.into();
    }

    match error {
        // The source was fine, so this is the parser running off the end of a
        // file that lied about its own structure.
        DemuxError::IoError(inner) => ExtractorError::Malformed {
            detail: inner.to_string(),
        },
        DemuxError::UnsupportedDocType(_) | DemuxError::UnsupportedDocTypeReadVersion(_) => {
            ExtractorError::Unsupported {
                detail: error.to_string(),
            }
        }
        other => ExtractorError::Malformed {
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::byte_source::InMemoryByteSource;

    // Real containers, not a hand-built element tree. See fixtures/README.md.
    const H264: &[u8] = include_bytes!("../../fixtures/h264.mkv");
    const HEVC: &[u8] = include_bytes!("../../fixtures/hevc.mkv");
    const H264_AAC: &[u8] = include_bytes!("../../fixtures/h264_aac.mkv");
    const VP9_OPUS: &[u8] = include_bytes!("../../fixtures/vp9_opus.webm");

    fn extractor(bytes: &[u8]) -> Arc<MatroskaExtractor> {
        MatroskaExtractor::open(Arc::new(InMemoryByteSource::new(bytes.to_vec())))
            .expect("fixture should open")
    }

    fn drain(extractor: &MatroskaExtractor) -> Vec<EncodedSample> {
        let mut samples = Vec::new();
        while let Some(sample) = extractor.next_sample().expect("read should succeed") {
            samples.push(sample);
        }
        samples
    }

    #[test]
    fn a_single_video_track_is_described_from_the_container() {
        let extractor = extractor(H264);
        let tracks = extractor.tracks();

        assert_eq!(tracks.len(), 1);
        let video = &tracks[0];
        assert_eq!(video.kind, TrackKind::Video);
        assert_eq!(video.video_codec, Some(VideoCodec::H264));
        assert_eq!(video.audio_codec, None);
        assert_eq!((video.width, video.height), (64, 64));
    }

    #[test]
    fn codec_private_bytes_are_handed_over_verbatim() {
        // The platform builds its format description from these; an extractor
        // that dropped or reinterpreted them would leave a decoder that cannot
        // be configured, which surfaces as a black picture rather than as an
        // error.
        let h264 = extractor(H264);
        let avcc = &h264.tracks()[0].codec_private;
        assert!(!avcc.is_empty(), "H.264 track carried no avcC");
        // An avcC record opens with configurationVersion == 1.
        assert_eq!(avcc[0], 1);

        let hevc = extractor(HEVC);
        let hvcc = &hevc.tracks()[0].codec_private;
        assert!(!hvcc.is_empty(), "HEVC track carried no hvcC");
        assert_eq!(hevc.tracks()[0].video_codec, Some(VideoCodec::H265));
    }

    #[test]
    fn a_two_track_file_describes_both_kinds() {
        let extractor = extractor(H264_AAC);
        let tracks = extractor.tracks();

        assert_eq!(tracks.len(), 2);
        let video = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .expect("no video track");
        let audio = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .expect("no audio track");

        assert_eq!(video.video_codec, Some(VideoCodec::H264));
        assert_eq!(audio.audio_codec, Some(AudioCodec::Aac));
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.channels, 1);
        assert_ne!(video.number, audio.number);
    }

    #[test]
    fn webm_is_read_through_the_same_path_as_matroska() {
        // WebM is a Matroska subset. If this needed its own branch, the branch
        // would be the bug.
        let extractor = extractor(VP9_OPUS);
        let tracks = extractor.tracks();

        assert_eq!(tracks.len(), 2);
        assert!(
            tracks
                .iter()
                .any(|track| track.video_codec == Some(VideoCodec::Vp9))
        );
        assert!(
            tracks
                .iter()
                .any(|track| track.audio_codec == Some(AudioCodec::Opus))
        );
    }

    #[test]
    fn every_sample_belongs_to_a_declared_track_and_carries_bytes() {
        let extractor = extractor(H264_AAC);
        let declared: HashSet<u64> = extractor
            .tracks()
            .iter()
            .map(|track| track.number)
            .collect();

        let samples = drain(&extractor);

        assert!(!samples.is_empty(), "fixture yielded no samples");
        for sample in &samples {
            assert!(
                declared.contains(&sample.track),
                "sample on undeclared track {}",
                sample.track
            );
            assert!(!sample.data.is_empty(), "sample carried no bytes");
            assert!(sample.pts_seconds >= 0.0);
        }
    }

    #[test]
    fn presentation_timestamps_stay_within_the_declared_duration() {
        // Catches a units error in the timestamp-scale conversion, which would
        // otherwise show up as playback that runs a thousand times too fast.
        let extractor = extractor(H264_AAC);
        let duration = extractor
            .duration_seconds()
            .expect("fixture declares a duration");

        assert!(
            (0.5..2.0).contains(&duration),
            "duration {duration}s is not the fixture's ~0.6s"
        );
        for sample in drain(&extractor) {
            assert!(
                sample.pts_seconds <= duration + 0.5,
                "sample at {}s is past a {duration}s file",
                sample.pts_seconds
            );
        }
    }

    #[test]
    fn the_first_video_sample_is_one_a_decoder_can_start_from() {
        let extractor = extractor(H264);

        let first = extractor
            .next_sample()
            .expect("read should succeed")
            .expect("fixture yielded no samples");

        assert!(first.is_keyframe);
    }

    #[test]
    fn selecting_one_track_excludes_the_other() {
        let extractor = extractor(H264_AAC);
        let audio = extractor
            .tracks()
            .into_iter()
            .find(|track| track.kind == TrackKind::Audio)
            .expect("no audio track");

        extractor.select_tracks(vec![audio.number]);

        let samples = drain(&extractor);
        assert!(!samples.is_empty(), "audio-only selection yielded nothing");
        assert!(
            samples.iter().all(|sample| sample.track == audio.number),
            "a deselected track still produced samples"
        );
    }

    #[test]
    fn selecting_no_tracks_yields_no_samples_rather_than_all_of_them() {
        // An empty selection is a caller's bug. Reading it as "everything"
        // would hide that bug behind every track playing at once.
        let extractor = extractor(H264_AAC);

        extractor.select_tracks(Vec::new());

        assert_eq!(extractor.next_sample().expect("read should succeed"), None);
    }

    #[test]
    fn a_seek_lands_on_a_keyframe_at_or_before_the_requested_position() {
        // The reason `seek` does not simply forward to the container: Matroska
        // positions at the first frame at or after a timestamp, which is
        // usually not a keyframe, and a decoder handed one renders nothing.
        let extractor = extractor(H264);
        let duration = extractor.duration_seconds().unwrap_or(0.6);
        let target = duration / 2.0;

        let landed = extractor.seek(target).expect("seek should succeed");

        assert!(
            landed <= target + f64::EPSILON,
            "seek landed at {landed}s, past the requested {target}s"
        );
        let next = extractor
            .next_sample()
            .expect("read should succeed")
            .expect("no sample after seek");
        assert!(next.is_keyframe, "the sample after a seek is not decodable");
        assert!((next.pts_seconds - landed).abs() < 0.05);
    }

    #[test]
    fn seeking_to_the_start_rewinds_to_the_first_sample() {
        let extractor = extractor(H264);
        let first = extractor
            .next_sample()
            .expect("read should succeed")
            .expect("fixture yielded no samples");
        drain(&extractor);

        let landed = extractor.seek(0.0).expect("seek should succeed");
        let after = extractor
            .next_sample()
            .expect("read should succeed")
            .expect("nothing after rewinding");

        assert_eq!(landed, 0.0);
        assert_eq!(after.pts_seconds, first.pts_seconds);
        assert_eq!(after.data, first.data);
    }

    #[test]
    fn a_negative_seek_is_treated_as_the_start_rather_than_refused() {
        // A player subtracting a skip interval near zero produces this
        // routinely; failing the call would turn "skip back 10s at 3s" into an
        // error dialog.
        let extractor = extractor(H264);

        assert_eq!(extractor.seek(-5.0).expect("seek should succeed"), 0.0);
    }

    #[test]
    fn seeking_past_the_end_reports_end_of_file_rather_than_failing() {
        let extractor = extractor(H264);

        extractor.seek(3_600.0).expect("seek should succeed");

        assert_eq!(extractor.next_sample().expect("read should succeed"), None);
    }

    #[test]
    fn an_audio_only_selection_seeks_exactly_because_every_frame_decodes() {
        let extractor = extractor(H264_AAC);
        let audio = extractor
            .tracks()
            .into_iter()
            .find(|track| track.kind == TrackKind::Audio)
            .expect("no audio track");
        extractor.select_tracks(vec![audio.number]);

        let landed = extractor.seek(0.3).expect("seek should succeed");

        assert!((landed - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn bytes_that_are_not_a_container_are_refused_rather_than_parsed() {
        let source = Arc::new(InMemoryByteSource::new(vec![0_u8; 4096]));

        let error = MatroskaExtractor::open(source).expect_err("expected refusal");

        assert!(
            matches!(error, ExtractorError::Malformed { .. }),
            "expected Malformed, got {error:?}"
        );
    }

    #[test]
    fn an_empty_source_is_refused_rather_than_opening_an_empty_file() {
        let source = Arc::new(InMemoryByteSource::new(Vec::new()));

        assert!(MatroskaExtractor::open(source).is_err());
    }

    #[test]
    fn a_failing_byte_source_is_reported_as_a_source_failure_not_a_corrupt_file() {
        // The distinction this asserts is the whole reason the reader keeps a
        // fault log: both arrive at the parser as an unexpected EOF, but one is
        // worth retrying and the other never is, and the UI says different
        // things. Compare against the truncated-container case below, which
        // produces the same io::Error and must classify the other way.
        let source = Arc::new(InMemoryByteSource::failing_after(H264_AAC.to_vec(), 0));

        let outcome = MatroskaExtractor::open(source).and_then(|extractor| {
            while extractor.next_sample()?.is_some() {}
            Ok(())
        });

        match outcome {
            Err(ExtractorError::Source { .. }) => {}
            other => panic!("expected a source failure, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_container_fails_rather_than_reporting_a_short_file() {
        // Half a file parses far enough to look valid, so this is the case a
        // length check alone would miss.
        let half = H264_AAC.len() / 2;
        let source = Arc::new(InMemoryByteSource::new(H264_AAC[..half].to_vec()));

        let outcome = MatroskaExtractor::open(source).and_then(|extractor| {
            while extractor.next_sample()?.is_some() {}
            Ok(())
        });

        match outcome {
            Err(ExtractorError::Malformed { .. }) => {}
            other => panic!("expected a malformed container, got {other:?}"),
        }
    }

    #[test]
    fn every_container_this_extractor_reports_is_one_it_can_actually_open() {
        // The list feeds the platform's DeviceProfile, and a container claimed
        // here but unopenable would have `select_source` offer a file that
        // cannot play. Derived from the fixtures rather than restated.
        let containers = probe_containers();

        for (bytes, extension) in [(H264, "mkv"), (VP9_OPUS, "webm")] {
            assert!(
                containers.contains(&extension.to_owned()),
                "{extension} is openable but unreported"
            );
            assert!(
                MatroskaExtractor::open(Arc::new(InMemoryByteSource::new(bytes.to_vec()))).is_ok(),
                "{extension} is reported but did not open"
            );
        }
    }

    #[test]
    fn subtitle_codec_ids_map_to_their_formats() {
        for (codec_id, expected) in [
            ("S_TEXT/UTF8", SubtitleFormat::SubRip),
            ("S_TEXT/ASS", SubtitleFormat::Ass),
            ("S_TEXT/WEBVTT", SubtitleFormat::WebVtt),
            ("S_HDMV/PGS", SubtitleFormat::Pgs),
            ("S_VOBSUB", SubtitleFormat::VobSub),
            ("S_SOMETHING/ELSE", SubtitleFormat::Unknown),
        ] {
            assert_eq!(subtitle_format(codec_id), expected);
        }
    }

    #[test]
    fn a_frame_with_no_keyframe_flag_is_treated_as_decodable() {
        // BlockGroup frames carry no flag. Assuming "not a keyframe" would
        // mean a seek finds no start point and never renders; assuming
        // "keyframe" costs at most one corrupt picture.
        let mut frame = Frame {
            is_keyframe: None,
            ..Frame::default()
        };
        assert!(is_keyframe(&frame));

        frame.is_keyframe = Some(false);
        assert!(!is_keyframe(&frame));
    }
}
