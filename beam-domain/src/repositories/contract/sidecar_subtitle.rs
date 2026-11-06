/// Behavioural contract for [`crate::repositories::SidecarSubtitleRepository`]
/// (issue #184): one row per subtitle path, upserted in place, listed by file
/// and by library in path order, and deleted by id.
///
/// `$setup` names an `async fn() -> impl SidecarSubtitleFixture`.
#[macro_export]
macro_rules! sidecar_subtitle_repository_contract {
    ($setup:path) => {
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
        use $crate::repositories::contract::fixture::SidecarSubtitleFixture;

        /// A fixed, non-epoch instant a whole second survives Postgres's
        /// microsecond precision at.
        fn at(offset_secs: i64) -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::DateTime::from_timestamp(1_700_000_000 + offset_secs, 0)
                .expect("valid instant")
        }

        /// A subtitle of `file_id` at a path of its own -- a fresh UUID keeps
        /// concurrently running Postgres tests apart -- named `name`.
        fn subtitle(
            library_id: Uuid,
            file_id: Uuid,
            dir: &str,
            name: &str,
        ) -> UpsertSidecarSubtitle {
            UpsertSidecarSubtitle {
                file_id,
                library_id,
                path: PathBuf::from(format!("/videos/{dir}/{name}")),
                info: SidecarInfo {
                    format: SubtitleFormat::Srt,
                    language: Some("eng".to_string()),
                    title: None,
                    is_forced: false,
                    is_sdh: false,
                    is_default: false,
                },
                size_bytes: 100,
                mtime: Some(at(0)),
            }
        }

        /// `row` holds every field `upsert` carries, each asserted on its own
        /// so a failure names the field.
        fn assert_records(
            upsert: &UpsertSidecarSubtitle,
            row: &$crate::models::sidecar::SidecarSubtitle,
        ) {
            let $crate::models::sidecar::SidecarSubtitle {
                id: _,
                file_id,
                library_id,
                path,
                info,
                size_bytes,
                mtime,
                created_at: _,
                updated_at: _,
            } = row;
            assert_eq!(*file_id, upsert.file_id, "file_id");
            assert_eq!(*library_id, upsert.library_id, "library_id");
            assert_eq!(*path, upsert.path, "path");
            assert_eq!(*info, upsert.info, "info");
            assert_eq!(*size_bytes, upsert.size_bytes, "size_bytes");
            assert_eq!(*mtime, upsert.mtime, "mtime");
        }

        #[tokio::test]
        async fn an_upsert_inserts_once_then_updates_the_row_at_its_path_in_place() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let other_video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let first = subtitle(library, video, &dir, "Movie.en.srt");

            let inserted = repo.upsert_by_path(first.clone()).await.unwrap();
            assert_records(&first, &inserted);
            assert_eq!(
                repo.find_by_path(&first.path).await.unwrap().map(|r| r.id),
                Some(inserted.id)
            );

            let changed = UpsertSidecarSubtitle {
                file_id: other_video,
                info: SidecarInfo {
                    format: SubtitleFormat::Ass,
                    language: None,
                    title: Some("Commentary".to_string()),
                    is_forced: true,
                    is_sdh: true,
                    is_default: true,
                },
                size_bytes: 250,
                mtime: None,
                ..first.clone()
            };
            let updated = repo.upsert_by_path(changed.clone()).await.unwrap();
            assert_eq!(updated.id, inserted.id, "one row per path, kept in place");
            assert_eq!(updated.created_at, inserted.created_at);
            assert_records(&changed, &updated);
            let stored = repo.find_by_path(&first.path).await.unwrap().unwrap();
            assert_records(&changed, &stored);
            assert!(repo.find_by_file_id(video).await.unwrap().is_empty());
            assert_eq!(repo.find_by_file_id(other_video).await.unwrap().len(), 1);
        }

        /// `sidecar_subtitles.mtime` is a `TIMESTAMPTZ`, as `files.mtime` is:
        /// a filesystem's nanoseconds read back as whole microseconds
        /// (issue #229), which is what a scan compares a subtitle's stat with.
        #[tokio::test]
        async fn an_mtime_reads_back_in_whole_microseconds() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let written = ::chrono::DateTime::from_timestamp(1_790_000_341, 802_029_432)
                .expect("valid instant");
            let read_back = ::chrono::DateTime::from_timestamp(1_790_000_341, 802_029_000)
                .expect("valid instant");

            let upserted = repo
                .upsert_by_path(UpsertSidecarSubtitle {
                    mtime: Some(written),
                    ..subtitle(library, video, &dir, "Movie.en.srt")
                })
                .await
                .unwrap();
            assert_eq!(upserted.mtime, Some(read_back));
            let stored = repo.find_by_path(&upserted.path).await.unwrap().unwrap();
            assert_eq!(stored.mtime, Some(read_back));
        }

        #[tokio::test]
        async fn subtitles_are_listed_by_file_and_by_library_in_path_order() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let sibling = fixture.new_video_file(library).await;
            let elsewhere = fixture.new_video_file(other_library).await;
            let dir = Uuid::new_v4().to_string();
            for upsert in [
                subtitle(library, video, &dir, "Movie.fr.srt"),
                subtitle(library, video, &dir, "Movie.en.srt"),
                subtitle(library, sibling, &dir, "Other.en.srt"),
                subtitle(other_library, elsewhere, &dir, "Elsewhere.en.srt"),
            ] {
                repo.upsert_by_path(upsert).await.unwrap();
            }
            let names = |rows: Vec<$crate::models::sidecar::SidecarSubtitle>| {
                rows.into_iter()
                    .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                names(repo.find_by_file_id(video).await.unwrap()),
                vec!["Movie.en.srt", "Movie.fr.srt"]
            );
            assert_eq!(
                names(repo.find_all_by_library(library).await.unwrap()),
                vec!["Movie.en.srt", "Movie.fr.srt", "Other.en.srt"]
            );
            assert_eq!(
                names(repo.find_all_by_library(other_library).await.unwrap()),
                vec!["Elsewhere.en.srt"]
            );
        }

        #[tokio::test]
        async fn delete_by_ids_removes_exactly_those_rows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let video = fixture.new_video_file(library).await;
            let dir = Uuid::new_v4().to_string();
            let keep = repo
                .upsert_by_path(subtitle(library, video, &dir, "Movie.en.srt"))
                .await
                .unwrap();
            let gone = repo
                .upsert_by_path(subtitle(library, video, &dir, "Movie.de.srt"))
                .await
                .unwrap();

            assert_eq!(repo.delete_by_ids(Vec::new()).await.unwrap(), 0);
            assert_eq!(
                repo.delete_by_ids(vec![gone.id, Uuid::new_v4()])
                    .await
                    .unwrap(),
                1,
                "an unknown id deletes nothing"
            );
            assert_eq!(repo.find_by_path(&gone.path).await.unwrap(), None);
            assert_eq!(
                repo.find_by_file_id(video)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![keep.id]
            );
        }

        #[tokio::test]
        async fn an_unknown_path_finds_nothing() {
            let fixture = $setup().await;
            let path = PathBuf::from(format!("/videos/{}/None.srt", Uuid::new_v4()));
            assert_eq!(fixture.repo().find_by_path(&path).await.unwrap(), None);
        }
    };
}
