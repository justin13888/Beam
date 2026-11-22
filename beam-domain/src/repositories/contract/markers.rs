/// Behavioural contract for [`crate::repositories::MarkerRepository`]
/// (issue #332): a file's kept markers read back in start order with who set
/// them; an administrator's marker replaces the one of its kind and no
/// detection replaces it; a clear removes one kind whoever set it; a
/// detection replaces the file's detected markers whole and remembers the
/// version it heard; and files are kept apart.
///
/// `$setup` names an `async fn() -> impl MarkerFixture`.
#[macro_export]
macro_rules! marker_repository_contract {
    ($setup:path) => {
        use $crate::models::chapter::{
            MarkerDetection, MarkerKind, PlaybackMarker, StoredMarker, StoredMarkerSource,
        };
        use $crate::repositories::contract::fixture::MarkerFixture;

        fn marker(kind: MarkerKind, start_ms: u64, end_ms: u64) -> PlaybackMarker {
            PlaybackMarker {
                kind,
                start_ms,
                end_ms,
            }
        }

        fn kept(marker: PlaybackMarker, source: StoredMarkerSource) -> StoredMarker {
            StoredMarker { marker, source }
        }

        fn detection(
            file_id: ::uuid::Uuid,
            file_hash: u64,
            markers: Vec<PlaybackMarker>,
        ) -> MarkerDetection {
            MarkerDetection {
                file_id,
                file_hash,
                markers,
                detected_at: ::chrono::DateTime::from_timestamp(1_800_000_000, 0)
                    .expect("an instant"),
            }
        }

        #[tokio::test]
        async fn a_files_markers_read_back_in_start_order_with_who_set_them() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let credits = marker(MarkerKind::Credits, 1_200_000, 1_290_000);
            let intro = marker(MarkerKind::Intro, 60_000, 150_000);

            repo.set_admin(file, credits).await.unwrap();
            repo.record_detection(detection(file, 7, vec![intro]))
                .await
                .unwrap();

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![
                    kept(intro, StoredMarkerSource::Detected),
                    kept(credits, StoredMarkerSource::Admin),
                ]
            );
            assert!(
                repo.find_by_file_id(::uuid::Uuid::new_v4())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn an_administrators_marker_replaces_its_kind_and_no_detection_replaces_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            repo.record_detection(detection(
                file,
                7,
                vec![marker(MarkerKind::Intro, 60_000, 150_000)],
            ))
            .await
            .unwrap();

            repo.set_admin(file, marker(MarkerKind::Intro, 62_000, 148_000))
                .await
                .unwrap();
            // A correction replaces the administrator's own marker too.
            repo.set_admin(file, marker(MarkerKind::Intro, 61_000, 149_000))
                .await
                .unwrap();
            repo.record_detection(detection(
                file,
                8,
                vec![
                    marker(MarkerKind::Intro, 0, 90_000),
                    marker(MarkerKind::Credits, 1_200_000, 1_290_000),
                ],
            ))
            .await
            .unwrap();

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![
                    kept(
                        marker(MarkerKind::Intro, 61_000, 149_000),
                        StoredMarkerSource::Admin
                    ),
                    kept(
                        marker(MarkerKind::Credits, 1_200_000, 1_290_000),
                        StoredMarkerSource::Detected
                    ),
                ]
            );
        }

        #[tokio::test]
        async fn a_clear_removes_one_kind_whoever_set_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let credits = marker(MarkerKind::Credits, 1_200_000, 1_290_000);
            repo.record_detection(detection(
                file,
                7,
                vec![marker(MarkerKind::Intro, 60_000, 150_000), credits],
            ))
            .await
            .unwrap();
            repo.set_admin(file, marker(MarkerKind::Recap, 0, 30_000))
                .await
                .unwrap();

            assert!(repo.clear(file, MarkerKind::Intro).await.unwrap());
            assert!(repo.clear(file, MarkerKind::Recap).await.unwrap());
            assert!(!repo.clear(file, MarkerKind::Preview).await.unwrap());
            assert!(
                !repo
                    .clear(::uuid::Uuid::new_v4(), MarkerKind::Intro)
                    .await
                    .unwrap()
            );

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![kept(credits, StoredMarkerSource::Detected)]
            );
        }

        #[tokio::test]
        async fn a_detection_replaces_the_detected_markers_whole_and_remembers_its_version() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let never = fixture.new_file().await;
            repo.record_detection(detection(
                file,
                7,
                vec![
                    marker(MarkerKind::Intro, 60_000, 150_000),
                    marker(MarkerKind::Credits, 1_200_000, 1_290_000),
                ],
            ))
            .await
            .unwrap();

            // The file changed, and this time only its intro was heard.
            repo.record_detection(detection(
                file,
                u64::MAX,
                vec![marker(MarkerKind::Intro, 30_000, 120_000)],
            ))
            .await
            .unwrap();

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![kept(
                    marker(MarkerKind::Intro, 30_000, 120_000),
                    StoredMarkerSource::Detected
                )]
            );
            let hashes = repo.detected_hashes(&[file, never]).await.unwrap();
            assert_eq!(hashes.len(), 1, "{hashes:?}");
            assert_eq!(hashes.get(&file), Some(&u64::MAX));

            // A detection that heard nothing is remembered all the same.
            repo.record_detection(detection(never, 3, Vec::new()))
                .await
                .unwrap();
            assert!(repo.find_by_file_id(never).await.unwrap().is_empty());
            assert_eq!(
                repo.detected_hashes(&[never]).await.unwrap().get(&never),
                Some(&3)
            );
            assert!(repo.detected_hashes(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn files_are_kept_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let other = fixture.new_file().await;
            let mine = marker(MarkerKind::Intro, 60_000, 150_000);
            repo.set_admin(file, mine).await.unwrap();
            repo.set_admin(other, marker(MarkerKind::Intro, 1_000, 2_000))
                .await
                .unwrap();
            repo.record_detection(detection(other, 9, Vec::new()))
                .await
                .unwrap();

            assert!(repo.clear(other, MarkerKind::Intro).await.unwrap());

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![kept(mine, StoredMarkerSource::Admin)]
            );
            assert!(repo.detected_hashes(&[file]).await.unwrap().is_empty());
        }
    };
}
