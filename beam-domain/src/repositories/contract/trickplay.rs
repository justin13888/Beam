/// Behavioural contract for [`crate::repositories::TrickplayRepository`]
/// (issue #265): a record reads back as written -- its layout, or the
/// absence of one -- one record per file, the later write winning, and
/// files are found together by id.
///
/// `$setup` names an `async fn() -> impl TrickplayFixture`.
#[macro_export]
macro_rules! trickplay_repository_contract {
    ($setup:path) => {
        use $crate::models::trickplay::{TrickplayLayout, TrickplayRecord};
        use $crate::repositories::contract::fixture::TrickplayFixture;

        /// A fixed, non-epoch instant a whole second survives Postgres's
        /// microsecond precision at.
        fn at(offset_secs: i64) -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::DateTime::from_timestamp(1_700_000_000 + offset_secs, 0)
                .expect("valid instant")
        }

        fn ready(file_id: ::uuid::Uuid, file_hash: u64, tile_count: u32) -> TrickplayRecord {
            TrickplayRecord {
                file_id,
                file_hash,
                layout: Some(TrickplayLayout {
                    interval_ms: 10_000,
                    tile_width: 320,
                    tile_height: 134,
                    columns: 10,
                    rows: 10,
                    tile_count,
                }),
                generated_at: at(0),
            }
        }

        #[tokio::test]
        async fn a_record_reads_back_as_written() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            // The top bit set: a hash is a `u64`, which the store must not
            // read back as some other number.
            let record = ready(file, u64::MAX - 7, 721);

            repo.upsert(record.clone()).await.unwrap();

            assert_eq!(repo.find(file).await.unwrap(), Some(record));
        }

        #[tokio::test]
        async fn a_file_that_gave_no_sheets_reads_back_with_no_layout() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let record = TrickplayRecord {
                file_id: file,
                file_hash: 42,
                layout: None,
                generated_at: at(5),
            };

            repo.upsert(record.clone()).await.unwrap();

            assert_eq!(repo.find(file).await.unwrap(), Some(record));
        }

        #[tokio::test]
        async fn a_file_has_one_record_and_the_later_write_wins() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            repo.upsert(ready(file, 1, 10)).await.unwrap();
            let later = TrickplayRecord {
                generated_at: at(60),
                ..ready(file, 2, 12)
            };

            repo.upsert(later.clone()).await.unwrap();

            assert_eq!(repo.find(file).await.unwrap(), Some(later.clone()));
            assert_eq!(repo.find_by_file_ids(&[file]).await.unwrap(), vec![later]);
        }

        #[tokio::test]
        async fn records_are_found_together_by_file_and_only_for_files_that_have_one() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let first = fixture.new_file().await;
            let second = fixture.new_file().await;
            let without = fixture.new_file().await;
            repo.upsert(ready(first, 1, 1)).await.unwrap();
            repo.upsert(ready(second, 2, 2)).await.unwrap();

            let mut found = repo
                .find_by_file_ids(&[first, without, second])
                .await
                .unwrap();
            found.sort_by_key(|record| record.file_hash);

            assert_eq!(found, vec![ready(first, 1, 1), ready(second, 2, 2)]);
            assert!(repo.find_by_file_ids(&[]).await.unwrap().is_empty());
            assert_eq!(repo.find(without).await.unwrap(), None);
        }
    };
}
