/// Behavioural contract for [`crate::repositories::ChapterRepository`]
/// (issue #265): a file's chapters read back as they were written, by index;
/// a replace is whole, so chapters the new set lacks are gone; an empty set
/// leaves none; and files are kept apart.
///
/// `$setup` names an `async fn() -> impl ChapterFixture`.
#[macro_export]
macro_rules! chapter_repository_contract {
    ($setup:path) => {
        use $crate::models::chapter::Chapter;
        use $crate::repositories::contract::fixture::ChapterFixture;

        fn chapter(index: u32, start_ms: u64, end_ms: u64, title: Option<&str>) -> Chapter {
            Chapter {
                index,
                start_ms,
                end_ms,
                title: title.map(str::to_owned),
            }
        }

        #[tokio::test]
        async fn a_files_chapters_read_back_by_index_as_written() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let written = vec![
                chapter(1, 90_000, 1_200_000, Some("Part A")),
                chapter(0, 0, 90_000, Some("Opening")),
                chapter(2, 1_200_000, 1_290_500, None),
            ];

            repo.replace_for_file(file, written).await.unwrap();

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![
                    chapter(0, 0, 90_000, Some("Opening")),
                    chapter(1, 90_000, 1_200_000, Some("Part A")),
                    chapter(2, 1_200_000, 1_290_500, None),
                ]
            );
        }

        #[tokio::test]
        async fn a_replace_is_whole_and_an_empty_one_leaves_none() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            repo.replace_for_file(
                file,
                vec![
                    chapter(0, 0, 10_000, Some("One")),
                    chapter(1, 10_000, 20_000, Some("Two")),
                    chapter(2, 20_000, 30_000, Some("Three")),
                ],
            )
            .await
            .unwrap();

            repo.replace_for_file(file, vec![chapter(0, 0, 30_000, Some("Whole"))])
                .await
                .unwrap();
            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![chapter(0, 0, 30_000, Some("Whole"))]
            );

            repo.replace_for_file(file, Vec::new()).await.unwrap();
            assert!(repo.find_by_file_id(file).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn files_are_kept_apart() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let other = fixture.new_file().await;
            repo.replace_for_file(file, vec![chapter(0, 0, 10_000, Some("Mine"))])
                .await
                .unwrap();
            repo.replace_for_file(other, vec![chapter(0, 0, 20_000, Some("Theirs"))])
                .await
                .unwrap();

            repo.replace_for_file(other, Vec::new()).await.unwrap();

            assert_eq!(
                repo.find_by_file_id(file).await.unwrap(),
                vec![chapter(0, 0, 10_000, Some("Mine"))]
            );
            assert!(
                repo.find_by_file_id(::uuid::Uuid::new_v4())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    };
}
