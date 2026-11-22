use super::*;
use beam_domain::repositories::MediaStreamRepository;

use crate::repositories::SqlMediaStreamRepository;

#[tokio::test]
async fn find_by_file_id_is_scoped_to_the_file() {
    let db = connection(empty_mock());
    let repo = SqlMediaStreamRepository::new(db.clone());
    let file = Uuid::from_u128(61);
    let _ = repo.find_by_file_id(file).await;
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "media_streams", "file_id", "=");
    assert_bound(&sql[0], &file.to_string());
}

#[tokio::test]
async fn delete_by_file_id_deletes_only_that_files_streams() {
    let db = connection(empty_mock());
    let repo = SqlMediaStreamRepository::new(db.clone());
    let file = Uuid::from_u128(62);
    let _ = repo.delete_by_file_id(file).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "DELETE FROM");
    assert_filters(&sql[0], "media_streams", "file_id", "=");
    assert_bound(&sql[0], &file.to_string());
}

/// A subtitle's hearing-impaired flag reaches its own column (#189): the
/// insert names the column and binds it the flag, distinct from the
/// default and forced flags beside it.
#[tokio::test]
async fn a_subtitle_insert_binds_its_hearing_impaired_flag() {
    use beam_domain::models::stream::SubtitleStreamMetadata;
    use beam_domain::models::{CreateMediaStream, StreamMetadata, StreamType};

    for is_hearing_impaired in [true, false] {
        let db = connection(empty_mock());
        let repo = SqlMediaStreamRepository::new(db.clone());
        let _ = repo
            .insert_streams(vec![CreateMediaStream {
                file_id: Uuid::from_u128(63),
                index: 2,
                stream_type: StreamType::Subtitle,
                codec: "subrip".to_string(),
                metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
                    language: None,
                    title: None,
                    is_default: !is_hearing_impaired,
                    is_forced: !is_hearing_impaired,
                    is_hearing_impaired,
                }),
            }])
            .await;
        drop(repo);

        let sql = statements(db);
        assert_contains(&sql[0], r#""is_hearing_impaired""#);
        let bools: Vec<String> = bound_values(&sql[0])
            .into_iter()
            .filter(|value| value.starts_with("Bool("))
            .collect();
        let set = bools.iter().filter(|value| value.contains("true")).count();
        assert_eq!(
            set,
            if is_hearing_impaired { 1 } else { 2 },
            "only the flags that are set bind true: {bools:?}"
        );
    }
}

#[tokio::test]
async fn insert_streams_with_nothing_to_insert_issues_no_statement() {
    let db = connection(empty_mock());
    let repo = SqlMediaStreamRepository::new(db.clone());
    let inserted = repo.insert_streams(Vec::new()).await.unwrap();
    drop(repo);

    assert_eq!(inserted, 0);
    assert!(
        statements(db).is_empty(),
        "an empty batch must not produce an `INSERT ... VALUES ()`"
    );
}
