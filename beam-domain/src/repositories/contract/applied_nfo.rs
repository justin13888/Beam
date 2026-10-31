/// Behavioural contract for [`crate::repositories::AppliedNfoRepository`]
/// (issue #184): one record per NFO path, recorded in place, listed by
/// library in path order, and deleted by id or by the directory above it.
///
/// `$setup` names an `async fn() -> impl AppliedNfoFixture`.
#[macro_export]
macro_rules! applied_nfo_repository_contract {
    ($setup:path) => {
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::applied_nfo::{AppliedNfo, RecordAppliedNfo};
        use $crate::repositories::contract::fixture::AppliedNfoFixture;

        /// The NFO `name` in a folder of its own -- a fresh UUID keeps
        /// concurrently running Postgres tests apart.
        fn nfo(library_id: Uuid, dir: &str, name: &str) -> RecordAppliedNfo {
            RecordAppliedNfo {
                library_id,
                path: PathBuf::from(format!("/videos/{dir}/{name}")),
                size_bytes: 120,
                content_hash: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string(),
                change_stamp: Some("120:1700000000000000000:1700000000000000000".to_string()),
            }
        }

        /// `row` holds every field `record` carries, each asserted on its own
        /// so a failure names the field.
        fn assert_records(record: &RecordAppliedNfo, row: &AppliedNfo) {
            let AppliedNfo {
                id: _,
                library_id,
                path,
                size_bytes,
                content_hash,
                change_stamp,
                created_at: _,
                updated_at: _,
            } = row;
            assert_eq!(*library_id, record.library_id, "library_id");
            assert_eq!(*path, record.path, "path");
            assert_eq!(*size_bytes, record.size_bytes, "size_bytes");
            assert_eq!(*content_hash, record.content_hash, "content_hash");
            assert_eq!(*change_stamp, record.change_stamp, "change_stamp");
        }

        #[tokio::test]
        async fn a_record_inserts_once_then_updates_the_row_at_its_path_in_place() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            let first = nfo(library, &dir, "movie.nfo");

            let inserted = repo.record_by_path(first.clone()).await.unwrap();
            assert_records(&first, &inserted);
            assert_eq!(
                repo.find_by_path(&first.path).await.unwrap().map(|r| r.id),
                Some(inserted.id)
            );

            let edited = RecordAppliedNfo {
                size_bytes: 64,
                content_hash: "ffeeddccbbaa99887766554433221100".to_string(),
                change_stamp: None,
                ..first.clone()
            };
            let updated = repo.record_by_path(edited.clone()).await.unwrap();
            assert_eq!(updated.id, inserted.id, "one row per path, kept in place");
            assert_eq!(updated.created_at, inserted.created_at);
            assert_records(&edited, &updated);
            let stored = repo.find_by_path(&first.path).await.unwrap().unwrap();
            assert_records(&edited, &stored);
        }

        #[tokio::test]
        async fn records_are_listed_by_library_in_path_order() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            for record in [
                nfo(library, &dir, "tvshow.nfo"),
                nfo(library, &dir, "movie.nfo"),
                nfo(other_library, &dir, "Elsewhere.nfo"),
            ] {
                repo.record_by_path(record).await.unwrap();
            }
            let names = |rows: Vec<AppliedNfo>| {
                rows.into_iter()
                    .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                names(repo.find_all_by_library(library).await.unwrap()),
                vec!["movie.nfo", "tvshow.nfo"]
            );
            assert_eq!(
                names(repo.find_all_by_library(other_library).await.unwrap()),
                vec!["Elsewhere.nfo"]
            );
        }

        #[tokio::test]
        async fn delete_by_ids_removes_exactly_those_rows() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let dir = Uuid::new_v4().to_string();
            let keep = repo
                .record_by_path(nfo(library, &dir, "movie.nfo"))
                .await
                .unwrap();
            let gone = repo
                .record_by_path(nfo(library, &dir, "tvshow.nfo"))
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
                repo.find_all_by_library(library)
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
            let path = PathBuf::from(format!("/videos/{}/movie.nfo", Uuid::new_v4()));
            assert_eq!(fixture.repo().find_by_path(&path).await.unwrap(), None);
        }

        /// The records beneath a directory are those whose path continues it
        /// by whole components, at any depth -- another library's never. A
        /// `_` or `%` in the directory's name is itself, not a wildcard.
        #[tokio::test]
        async fn delete_beneath_removes_the_records_beneath_a_directory_by_whole_components() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let base = Uuid::new_v4().to_string();
            let dir = format!("{base}/Show_%1");
            for gone in [
                nfo(library, &dir, "tvshow.nfo"),
                nfo(library, &format!("{dir}/Season 01"), "S01E01.nfo"),
            ] {
                repo.record_by_path(gone).await.unwrap();
            }
            // Longer by a character, what the wildcards would match, a
            // sibling file named like the directory, and another library.
            let kept = [
                nfo(library, &format!("{base}/Show_%10"), "tvshow.nfo"),
                nfo(library, &format!("{base}/ShowAB1"), "tvshow.nfo"),
                nfo(library, &base, "Show_%1.nfo"),
                nfo(other, &format!("{dir}/Season 02"), "S02E01.nfo"),
            ];
            for record in kept.clone() {
                repo.record_by_path(record).await.unwrap();
            }

            let dir_path = PathBuf::from(format!("/videos/{dir}"));
            assert_eq!(repo.delete_beneath(library, &dir_path).await.unwrap(), 2);

            let mut left: Vec<PathBuf> = repo
                .find_all_by_library(library)
                .await
                .unwrap()
                .into_iter()
                .chain(repo.find_all_by_library(other).await.unwrap())
                .map(|r| r.path)
                .collect();
            left.sort();
            let mut expected: Vec<PathBuf> = kept.into_iter().map(|r| r.path).collect();
            expected.sort();
            assert_eq!(left, expected);
            assert_eq!(
                repo.delete_beneath(library, &dir_path).await.unwrap(),
                0,
                "nothing is left beneath it"
            );
        }
    };
}
