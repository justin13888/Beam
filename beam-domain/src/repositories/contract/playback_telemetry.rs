/// Behavioural contract for [`crate::repositories::PlaybackTelemetryRepository`]
/// (issue #143).
///
/// `$setup` names an `async fn() -> impl PlaybackTelemetryFixture` whose
/// repository starts empty.
#[macro_export]
macro_rules! playback_telemetry_repository_contract {
    ($setup:path) => {
        use ::chrono::NaiveDate;
        use $crate::models::playback_telemetry::test_utils::{rebuffer_key, start_key, switch_key};
        use $crate::models::playback_telemetry::{
            BitrateClass, ClientKind, FailureReason, FailureStage, HeightClass,
            PlaybackTelemetryEvent, PlaybackTelemetrySummary, RebufferBucket, RebufferKey,
            StartKey, StartOutcome, SwitchKey, SwitchTrigger,
        };
        use $crate::repositories::contract::fixture::PlaybackTelemetryFixture;

        fn day(n: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 9, n).expect("a September day")
        }

        fn start_event(key: StartKey) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Start(key)
        }

        fn rebuffer_event(key: RebufferKey, duration_ms: u32) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Rebuffer { key, duration_ms }
        }

        fn switch_event(key: SwitchKey) -> PlaybackTelemetryEvent {
            PlaybackTelemetryEvent::Switch(key)
        }

        #[tokio::test]
        async fn a_start_recorded_twice_counts_two() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            assert_eq!(summary.starts.len(), 1);
            assert_eq!(summary.starts[0].key, start_key());
            assert_eq!(summary.starts[0].count, 2);
        }

        /// One batch mixing every kind, naming some keys more than once,
        /// reads back as its own tally -- every event counted once, repeats
        /// folded into one row -- and a second batch adds to what the first
        /// counted.
        #[tokio::test]
        async fn a_batch_counts_each_event_once_and_adds_to_what_is_kept() {
            let fixture = $setup().await;
            let hd = StartKey {
                height_class: HeightClass::Hd,
                ..start_key()
            };
            let batch = vec![
                PlaybackTelemetryEvent::Start(start_key()),
                PlaybackTelemetryEvent::Switch(switch_key()),
                PlaybackTelemetryEvent::Rebuffer {
                    key: rebuffer_key(),
                    duration_ms: 500,
                },
                PlaybackTelemetryEvent::Start(hd.clone()),
                PlaybackTelemetryEvent::Start(start_key()),
                PlaybackTelemetryEvent::Rebuffer {
                    key: rebuffer_key(),
                    duration_ms: 12_000,
                },
            ];

            fixture.repo().record_batch(day(1), &batch).await.unwrap();

            let once = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(once, PlaybackTelemetrySummary::tally(&batch));

            fixture.repo().record_batch(day(1), &batch).await.unwrap();

            let twice = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            let doubled: Vec<PlaybackTelemetryEvent> =
                batch.iter().chain(batch.iter()).cloned().collect();
            assert_eq!(twice, PlaybackTelemetrySummary::tally(&doubled));
        }

        #[tokio::test]
        async fn an_empty_batch_counts_nothing() {
            let fixture = $setup().await;

            fixture.repo().record_batch(day(1), &[]).await.unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(summary, Default::default());
        }

        /// Every dimension is part of the key: a start differing in any one
        /// of them is counted apart, and the list comes back sorted by key.
        #[tokio::test]
        async fn every_dimension_separates_starts() {
            let fixture = $setup().await;
            let variants = vec![
                start_key(),
                StartKey {
                    client_kind: ClientKind::Android,
                    ..start_key()
                },
                StartKey {
                    outcome: StartOutcome::Failed {
                        reason: FailureReason::VideoCodec,
                        stage: FailureStage::Preflight,
                    },
                    ..start_key()
                },
                StartKey {
                    outcome: StartOutcome::Failed {
                        reason: FailureReason::VideoCodec,
                        stage: FailureStage::Playback,
                    },
                    ..start_key()
                },
                StartKey {
                    container: "mov,mp4,m4a,3gp,3g2,mj2".to_string(),
                    ..start_key()
                },
                StartKey {
                    video_codec: "hevc".to_string(),
                    ..start_key()
                },
                StartKey {
                    audio_codec: "eac3".to_string(),
                    ..start_key()
                },
                StartKey {
                    height_class: HeightClass::Uhd,
                    ..start_key()
                },
            ];
            for key in variants.iter().rev() {
                fixture
                    .repo()
                    .record_batch(day(1), &[start_event(key.clone())])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let mut expected = variants.clone();
            expected.sort();
            let keys: Vec<StartKey> = summary.starts.iter().map(|s| s.key.clone()).collect();
            assert_eq!(keys, expected);
            assert!(summary.starts.iter().all(|s| s.count == 1));
        }

        /// Every value of every vocabulary survives the round trip through
        /// storage, so no label is written one way and read another.
        #[tokio::test]
        async fn every_vocabulary_value_round_trips() {
            let fixture = $setup().await;
            let mut starts = Vec::new();
            for client_kind in ClientKind::ALL {
                starts.push(StartKey {
                    client_kind: *client_kind,
                    ..start_key()
                });
            }
            for reason in FailureReason::ALL {
                for stage in FailureStage::ALL {
                    starts.push(StartKey {
                        outcome: StartOutcome::Failed {
                            reason: *reason,
                            stage: *stage,
                        },
                        ..start_key()
                    });
                }
            }
            for height_class in HeightClass::ALL {
                starts.push(StartKey {
                    height_class: *height_class,
                    ..start_key()
                });
            }
            starts.sort();
            starts.dedup();
            for key in &starts {
                fixture
                    .repo()
                    .record_batch(day(1), &[start_event(key.clone())])
                    .await
                    .unwrap();
            }
            let mut rebuffers = Vec::new();
            for bitrate_class in BitrateClass::ALL {
                rebuffers.push(RebufferKey {
                    bitrate_class: *bitrate_class,
                    ..rebuffer_key()
                });
            }
            for key in &rebuffers {
                fixture
                    .repo()
                    .record_batch(day(1), &[rebuffer_event(key.clone(), 10)])
                    .await
                    .unwrap();
            }
            let mut switches = Vec::new();
            for trigger in SwitchTrigger::ALL {
                for to_height_class in HeightClass::ALL {
                    switches.push(SwitchKey {
                        trigger: *trigger,
                        to_height_class: *to_height_class,
                        ..switch_key()
                    });
                }
            }
            for key in &switches {
                fixture
                    .repo()
                    .record_batch(day(1), &[switch_event(*key)])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let read: Vec<StartKey> = summary.starts.iter().map(|s| s.key.clone()).collect();
            assert_eq!(read, starts);
            let read: Vec<RebufferKey> = summary.rebuffers.iter().map(|r| r.key.clone()).collect();
            rebuffers.sort();
            assert_eq!(read, rebuffers);
            let read: Vec<SwitchKey> = summary.switches.iter().map(|s| s.key).collect();
            switches.sort();
            assert_eq!(read, switches);
        }

        /// A counter is kept per day, and a summary sums the days it spans.
        #[tokio::test]
        async fn counters_are_kept_per_day_and_summed_across_a_range() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(2), &[start_event(start_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(2), &[start_event(start_key())])
                .await
                .unwrap();

            let first = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            let second = fixture.repo().summarize(day(2), day(2)).await.unwrap();
            let both = fixture.repo().summarize(day(1), day(2)).await.unwrap();

            assert_eq!(first.starts[0].count, 1);
            assert_eq!(second.starts[0].count, 2);
            assert_eq!(both.starts.len(), 1, "one entry per key, not per day");
            assert_eq!(both.starts[0].count, 3);
        }

        /// Both ends of the range are included; a day either side is not.
        #[tokio::test]
        async fn a_summary_includes_both_ends_and_nothing_outside() {
            let fixture = $setup().await;
            for n in [1, 2, 4, 5] {
                fixture
                    .repo()
                    .record_batch(day(n), &[start_event(start_key())])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[rebuffer_event(rebuffer_key(), 100)])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[switch_event(switch_key())])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(2), day(4)).await.unwrap();

            assert_eq!(summary.starts[0].count, 2);
            assert_eq!(summary.rebuffers[0].events, 2);
            assert_eq!(summary.switches[0].count, 2);
            let gap = fixture.repo().summarize(day(3), day(3)).await.unwrap();
            assert_eq!(gap, Default::default());
            let backwards = fixture.repo().summarize(day(4), day(2)).await.unwrap();
            assert_eq!(backwards, Default::default());
        }

        /// A rebuffer adds one event, its duration to the total, and one to
        /// the range it falls into -- straddling a boundary lands on either
        /// side of it.
        #[tokio::test]
        async fn a_rebuffer_adds_its_duration_and_its_bucket() {
            let fixture = $setup().await;
            for duration_ms in [999, 1_000, 30_000] {
                fixture
                    .repo()
                    .record_batch(day(1), &[rebuffer_event(rebuffer_key(), duration_ms)])
                    .await
                    .unwrap();
            }

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let row = &summary.rebuffers[0];
            assert_eq!(row.key, rebuffer_key());
            assert_eq!(row.events, 3);
            assert_eq!(row.total_ms, 31_999);
            assert_eq!(row.histogram.count(RebufferBucket::Under1Secs), 1);
            assert_eq!(row.histogram.count(RebufferBucket::From1To3Secs), 1);
            assert_eq!(row.histogram.count(RebufferBucket::From3To10Secs), 0);
            assert_eq!(row.histogram.count(RebufferBucket::From10To30Secs), 0);
            assert_eq!(row.histogram.count(RebufferBucket::AtLeast30Secs), 1);
        }

        #[tokio::test]
        async fn a_switch_recorded_twice_counts_two() {
            let fixture = $setup().await;
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(switch_key())])
                .await
                .unwrap();
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(switch_key())])
                .await
                .unwrap();
            let auto = SwitchKey {
                trigger: SwitchTrigger::Auto,
                ..switch_key()
            };
            fixture
                .repo()
                .record_batch(day(1), &[switch_event(auto)])
                .await
                .unwrap();

            let summary = fixture.repo().summarize(day(1), day(1)).await.unwrap();

            let counts: Vec<(SwitchKey, u64)> =
                summary.switches.iter().map(|s| (s.key, s.count)).collect();
            let mut expected = vec![(switch_key(), 2), (auto, 1)];
            expected.sort();
            assert_eq!(counts, expected);
        }

        /// Pruning removes the days strictly before the cutoff, in every
        /// table, and counts the day-rows it removed.
        #[tokio::test]
        async fn pruning_removes_only_days_before_the_cutoff() {
            let fixture = $setup().await;
            for n in [1, 2, 3] {
                fixture
                    .repo()
                    .record_batch(day(n), &[start_event(start_key())])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[rebuffer_event(rebuffer_key(), 100)])
                    .await
                    .unwrap();
                fixture
                    .repo()
                    .record_batch(day(n), &[switch_event(switch_key())])
                    .await
                    .unwrap();
            }

            let removed = fixture.repo().prune_before(day(2)).await.unwrap();

            assert_eq!(removed, 3, "one row per table for the one day before");
            let before = fixture.repo().summarize(day(1), day(1)).await.unwrap();
            assert_eq!(before, Default::default());
            let kept = fixture.repo().summarize(day(2), day(3)).await.unwrap();
            assert_eq!(kept.starts[0].count, 2);
            assert_eq!(kept.rebuffers[0].events, 2);
            assert_eq!(kept.switches[0].count, 2);
            assert_eq!(fixture.repo().prune_before(day(2)).await.unwrap(), 0);
        }
    };
}
