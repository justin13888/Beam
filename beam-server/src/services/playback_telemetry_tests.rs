//! The pure rules of playback telemetry -- validation, file resolution, the
//! report's range and shape, the retention cutoff -- and the retention loop
//! over an in-memory store and a test clock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use beam_domain::models::file::{FileStatus, MediaFile};
use beam_domain::models::playback_telemetry::test_utils::{rebuffer_key, start_key, switch_key};
use beam_domain::models::playback_telemetry::{
    FailureReason, FailureStage, NO_STREAM_LABEL, PlaybackTelemetrySummary, RebufferCount,
    RebufferHistogram, StartCount, StartKey, StartOutcome, SwitchCount,
};
use beam_domain::models::stream::{
    AudioStreamMetadata, MediaStream, StreamMetadata, StreamType, VideoStreamMetadata,
};
use beam_domain::models::{BitrateClass, HeightClass};
use beam_domain::repositories::PlaybackTelemetryRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::playback_telemetry::in_memory::InMemoryPlaybackTelemetryRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::services::TestClock;
use beam_domain::utils::telemetry::UNKNOWN_LABEL;
use chrono::{Days, NaiveDate, TimeZone, Utc};
use proptest::prelude::*;
use uuid::Uuid;

use super::*;
use crate::models::playback_telemetry::{
    PlaybackClientKind, PlaybackFailureReason, PlaybackFailureStage, PlaybackRebufferEvent,
    PlaybackSourceSwitchEvent, PlaybackStartEvent, PlaybackSwitchTrigger,
};

fn batch() -> PlaybackTelemetryBatch {
    PlaybackTelemetryBatch {
        client_kind: PlaybackClientKind::Android,
        starts: vec![],
        start_failures: vec![],
        rebuffers: vec![],
        source_switches: vec![],
    }
}

fn start() -> PlaybackStartEvent {
    PlaybackStartEvent {
        file_id: Uuid::from_u128(1),
        audio_track_index: None,
    }
}

fn rebuffer(duration_ms: u32) -> PlaybackRebufferEvent {
    PlaybackRebufferEvent {
        file_id: Uuid::from_u128(1),
        duration_ms,
    }
}

fn switch(from: u128, to: u128) -> PlaybackSourceSwitchEvent {
    PlaybackSourceSwitchEvent {
        from_file_id: Uuid::from_u128(from),
        to_file_id: Uuid::from_u128(to),
        trigger: PlaybackSwitchTrigger::Manual,
    }
}

fn pointers(errors: &[FieldError]) -> Vec<&str> {
    errors.iter().map(|e| e.pointer.as_str()).collect()
}

// ── validate_batch ──────────────────────────────────────────────────────────

#[test]
fn a_batch_breaks_each_rule_at_its_own_pointer() {
    let cases: Vec<(&str, PlaybackTelemetryBatch, Vec<&str>)> = vec![
        (
            "one start",
            PlaybackTelemetryBatch {
                starts: vec![start()],
                ..batch()
            },
            vec![],
        ),
        ("empty", batch(), vec![""]),
        (
            "fifty starts",
            PlaybackTelemetryBatch {
                starts: vec![start(); MAX_BATCH_EVENTS],
                ..batch()
            },
            vec![],
        ),
        (
            "fifty-one starts",
            PlaybackTelemetryBatch {
                starts: vec![start(); MAX_BATCH_EVENTS + 1],
                ..batch()
            },
            vec!["/starts"],
        ),
        (
            "fifty-one across two lists",
            PlaybackTelemetryBatch {
                starts: vec![start(); 26],
                rebuffers: vec![rebuffer(10); 25],
                ..batch()
            },
            vec![""],
        ),
        (
            "rebuffer bounds",
            PlaybackTelemetryBatch {
                rebuffers: vec![
                    rebuffer(0),
                    rebuffer(1),
                    rebuffer(MAX_REBUFFER_MS),
                    rebuffer(MAX_REBUFFER_MS + 1),
                ],
                ..batch()
            },
            vec!["/rebuffers/0/duration_ms", "/rebuffers/3/duration_ms"],
        ),
        (
            "a switch to itself",
            PlaybackTelemetryBatch {
                source_switches: vec![switch(1, 2), switch(3, 3)],
                ..batch()
            },
            vec!["/source_switches/1/to_file_id"],
        ),
    ];
    for (name, batch, expected) in cases {
        assert_eq!(pointers(&validate_batch(&batch)), expected, "{name}");
    }
}

proptest! {
    /// Whatever the sizes and durations, validation never panics, and a batch
    /// passes exactly when every rule holds.
    #[test]
    fn a_batch_passes_exactly_when_every_rule_holds(
        starts in 0usize..60,
        durations in proptest::collection::vec(any::<u32>(), 0..60),
        switches in proptest::collection::vec((0u128..4, 0u128..4), 0..60),
    ) {
        let batch = PlaybackTelemetryBatch {
            starts: vec![start(); starts],
            rebuffers: durations.iter().map(|d| rebuffer(*d)).collect(),
            source_switches: switches.iter().map(|(f, t)| switch(*f, *t)).collect(),
            ..batch()
        };
        let total = batch.event_count();
        let valid = (1..=MAX_BATCH_EVENTS).contains(&total)
            && durations.iter().all(|d| (1..=MAX_REBUFFER_MS).contains(d))
            && switches.iter().all(|(f, t)| f != t);
        prop_assert_eq!(validate_batch(&batch).is_empty(), valid);
    }
}

// ── ResolvedFile ────────────────────────────────────────────────────────────

fn media_file(container: Option<&str>, size_bytes: u64, duration: Option<Duration>) -> MediaFile {
    MediaFile {
        id: Uuid::from_u128(9),
        library_id: Uuid::from_u128(8),
        path: PathBuf::from("/m/a.mkv"),
        hash: 0,
        size_bytes,
        mtime: None,
        mime_type: None,
        duration,
        container_format: container.map(str::to_owned),
        content: None,
        status: FileStatus::Known,
        scanned_at: Utc::now(),
        updated_at: Utc::now(),
        missing_since: None,
    }
}

fn video(index: u32, codec: &str, height: u32, bit_rate: Option<u64>) -> MediaStream {
    MediaStream {
        id: Uuid::new_v4(),
        file_id: Uuid::from_u128(9),
        index,
        stream_type: StreamType::Video,
        codec: codec.to_owned(),
        metadata: StreamMetadata::Video(VideoStreamMetadata {
            width: 0,
            height,
            frame_rate: None,
            bit_rate,
            color_space: None,
            color_range: None,
            hdr_format: None,
        }),
    }
}

fn audio(index: u32, codec: &str, is_default: bool) -> MediaStream {
    MediaStream {
        id: Uuid::new_v4(),
        file_id: Uuid::from_u128(9),
        index,
        stream_type: StreamType::Audio,
        codec: codec.to_owned(),
        metadata: StreamMetadata::Audio(AudioStreamMetadata {
            language: None,
            title: None,
            channels: 2,
            sample_rate: 48_000,
            channel_layout: None,
            bit_rate: None,
            is_default,
            is_forced: false,
        }),
    }
}

/// The first video stream by index supplies the codec and height, however
/// the streams arrive -- a cover-art stream later in the file does not.
#[test]
fn the_first_video_stream_by_index_is_the_one_counted() {
    let file = media_file(
        Some("Matroska,WebM"),
        1_000_000_000,
        Some(Duration::from_secs(1_000)),
    );
    let streams = [
        video(3, "mjpeg", 600, None),
        audio(1, "aac", false),
        video(0, "H264", 1080, None),
    ];

    let resolved = ResolvedFile::of(&file, &streams);

    assert_eq!(resolved.container, "matroska,webm");
    assert_eq!(resolved.video_codec, "h264");
    assert_eq!(resolved.height_class, HeightClass::Fhd);
    assert_eq!(resolved.bitrate_class, BitrateClass::From8To20Mbps);
}

#[test]
fn a_file_with_no_streams_resolves_to_sentinels() {
    let resolved = ResolvedFile::of(&media_file(None, 0, None), &[]);

    assert_eq!(resolved.container, UNKNOWN_LABEL);
    assert_eq!(resolved.video_codec, NO_STREAM_LABEL);
    assert_eq!(resolved.height_class, HeightClass::Unknown);
    assert_eq!(resolved.bitrate_class, BitrateClass::Unknown);
    assert_eq!(resolved.audio_codec(None), NO_STREAM_LABEL);
    assert_eq!(resolved.audio_codec(Some(0)), NO_STREAM_LABEL);
}

/// Without a duration there is no file bitrate, so the video stream's own
/// stands in for it.
#[test]
fn without_a_duration_the_video_bitrate_classifies_the_file() {
    let resolved = ResolvedFile::of(
        &media_file(None, 1, None),
        &[video(0, "hevc", 2160, Some(60_000_000))],
    );

    assert_eq!(resolved.bitrate_class, BitrateClass::AtLeast50Mbps);
}

#[test]
fn the_audio_codec_is_the_chosen_track_else_the_default_else_the_first() {
    let file = media_file(None, 0, None);
    let with_default = ResolvedFile::of(
        &file,
        &[
            audio(2, "eac3", true),
            audio(1, "truehd", false),
            audio(3, "aac", false),
        ],
    );
    // Stream order: truehd (1), eac3 (2, default), aac (3).
    assert_eq!(with_default.audio_codec(Some(0)), "truehd");
    assert_eq!(with_default.audio_codec(Some(2)), "aac");
    assert_eq!(with_default.audio_codec(None), "eac3");
    assert_eq!(with_default.audio_codec(Some(3)), "eac3", "out of range");

    let without_default =
        ResolvedFile::of(&file, &[audio(1, "opus", false), audio(2, "aac", false)]);
    assert_eq!(without_default.audio_codec(None), "opus");
}

// ── report_range and retention_cutoff ───────────────────────────────────────

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

#[test]
fn a_report_range_defaults_to_the_last_thirty_days() {
    let today = day(2026, 9, 27);
    let (from, to) = report_range(today, None, None).unwrap();
    assert_eq!(to, today);
    assert_eq!((to - from).num_days() + 1, DEFAULT_REPORT_DAYS as i64);

    let (from, to) = report_range(today, None, Some(day(2026, 1, 30))).unwrap();
    assert_eq!((from, to), (day(2026, 1, 1), day(2026, 1, 30)));

    let (from, to) = report_range(today, Some(day(2026, 9, 1)), None).unwrap();
    assert_eq!((from, to), (day(2026, 9, 1), today));
}

#[test]
fn a_report_range_refuses_backwards_and_overlong_spans() {
    let today = day(2026, 9, 27);
    let single = report_range(today, Some(today), Some(today)).unwrap();
    assert_eq!(single, (today, today));
    let widest_from = today - Days::new(MAX_REPORT_DAYS - 1);
    assert!(report_range(today, Some(widest_from), Some(today)).is_ok());
    for (from, to) in [
        (today + Days::new(1), today),
        (widest_from - Days::new(1), today),
    ] {
        assert!(
            matches!(
                report_range(today, Some(from), Some(to)),
                Err(ReportError::InvalidDateRange(_))
            ),
            "{from}..{to}"
        );
    }
}

#[test]
fn the_retention_cutoff_keeps_exactly_the_retention_period() {
    let today = day(2026, 9, 27);
    assert_eq!(retention_cutoff(today, 1), day(2026, 9, 26));
    assert_eq!(retention_cutoff(today, 365), day(2025, 9, 27));
}

// ── build_report ────────────────────────────────────────────────────────────

fn config() -> PlaybackTelemetryConfig {
    PlaybackTelemetryConfig {
        enabled: true,
        retention_days: 90,
    }
}

/// Successes and failures land in their own lists, every list is largest
/// first, and the totals are the sums of the rows.
#[test]
fn a_report_splits_outcomes_sorts_largest_first_and_totals() {
    let failed = StartOutcome::Failed {
        reason: FailureReason::AudioCodec,
        stage: FailureStage::Playback,
    };
    let summary = PlaybackTelemetrySummary {
        starts: vec![
            StartCount {
                key: start_key(),
                count: 2,
            },
            StartCount {
                key: StartKey {
                    height_class: HeightClass::Uhd,
                    ..start_key()
                },
                count: 7,
            },
            StartCount {
                key: StartKey {
                    outcome: failed,
                    ..start_key()
                },
                count: 3,
            },
        ],
        rebuffers: vec![RebufferCount {
            key: rebuffer_key(),
            events: 4,
            total_ms: 9_000,
            histogram: RebufferHistogram::from_counts([1, 1, 2, 0, 0]),
        }],
        switches: vec![SwitchCount {
            key: switch_key(),
            count: 5,
        }],
    };

    let report = build_report(summary, config(), day(2026, 9, 1), day(2026, 9, 27));

    let counts: Vec<u64> = report.starts.iter().map(|s| s.count).collect();
    assert_eq!(counts, vec![7, 2]);
    assert_eq!(report.start_failures.len(), 1);
    assert_eq!(
        report.start_failures[0].reason,
        PlaybackFailureReason::AudioCodec
    );
    assert_eq!(
        report.start_failures[0].stage,
        PlaybackFailureStage::Playback
    );
    assert_eq!(report.totals.started_count, 9);
    assert_eq!(report.totals.failed_count, 3);
    assert_eq!(report.totals.rebuffer_count, 4);
    assert_eq!(report.totals.rebuffer_total_ms, 9_000);
    assert_eq!(report.totals.source_switch_count, 5);
    let histogram: Vec<u64> = report.rebuffers[0]
        .histogram
        .iter()
        .map(|b| b.count)
        .collect();
    assert_eq!(
        histogram,
        vec![1, 1, 2, 0, 0],
        "every bucket, shortest first"
    );
    assert_eq!(report.retention_days, 90);
    assert!(report.enabled);
}

/// Rows with equal counts keep the summary's key order, so two reads of one
/// store read alike.
#[test]
fn equal_counts_keep_key_order() {
    let keys = [
        StartKey {
            height_class: HeightClass::Sd,
            ..start_key()
        },
        StartKey {
            height_class: HeightClass::Hd,
            ..start_key()
        },
    ];
    let summary = PlaybackTelemetrySummary {
        starts: keys
            .iter()
            .map(|key| StartCount {
                key: key.clone(),
                count: 1,
            })
            .collect(),
        ..Default::default()
    };

    let report = build_report(summary, config(), day(2026, 9, 1), day(2026, 9, 1));

    let heights: Vec<_> = report.starts.iter().map(|s| s.height_class).collect();
    assert_eq!(
        heights,
        vec![HeightClass::Sd.into(), HeightClass::Hd.into()]
    );
}

// ── Retention ───────────────────────────────────────────────────────────────

async fn until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for: {label}");
}

/// The loop prunes at once and then daily; each pass removes the day that
/// has just aged out and keeps the rest. It runs with collection disabled:
/// turning collection off does not stop old counts expiring.
#[tokio::test]
async fn retention_prunes_at_start_and_then_daily() {
    let clock = Arc::new(TestClock::starting_at(
        Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap(),
    ));
    let repo = Arc::new(InMemoryPlaybackTelemetryRepository::default());
    let today = day(2026, 9, 27);
    // Retention of 2 days keeps 25th..27th on the 27th.
    for n in [24, 25, 26] {
        repo.record_start(day(2026, 9, n), start_key())
            .await
            .unwrap();
    }
    let service = Arc::new(PlaybackTelemetryService::new(
        PlaybackTelemetryConfig {
            enabled: false,
            retention_days: 2,
        },
        repo.clone(),
        Arc::new(InMemoryFileRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        clock.clone(),
    ));
    let kept = |from: NaiveDate| {
        let repo = repo.clone();
        async move {
            repo.summarize(from, today)
                .await
                .unwrap()
                .starts
                .iter()
                .map(|s| s.count)
                .sum::<u64>()
        }
    };

    let running = service.clone();
    tokio::spawn(async move { running.run_retention().await });
    until("the first pass to sleep", || clock.waiter_count() == 1).await;
    assert_eq!(kept(day(2026, 9, 1)).await, 2, "the 24th is gone at start");

    // `advance` wakes the sleeper and removes it at once, so the next waiter
    // is the loop asleep again -- after its second pass.
    clock.advance(PRUNE_INTERVAL);
    until("the second pass to sleep", || clock.waiter_count() == 1).await;
    assert_eq!(kept(day(2026, 9, 1)).await, 1, "a day on, the 25th is gone");
}
