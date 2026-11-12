use super::*;

use beam_domain::models::chapter::Chapter;
use beam_domain::repositories::ChapterRepository;

use crate::repositories::SqlChapterRepository;

fn chapter(index: u32, start_ms: u64) -> Chapter {
    Chapter {
        index,
        start_ms,
        end_ms: start_ms + 1_000,
        title: Some(format!("Chapter {index}")),
    }
}

/// A replacement deletes only this file's chapters and inserts the new
/// set, in one transaction (issue #265).
#[tokio::test]
async fn a_replacement_deletes_the_files_chapters_then_inserts_the_new_set_in_one_transaction() {
    let file = Uuid::from_u128(0x265);
    let db = connection(empty_mock());
    let repo = SqlChapterRepository::new(db.clone());
    repo.replace_for_file(file, vec![chapter(0, 0), chapter(1, 1_000)])
        .await
        .unwrap();
    drop(repo);

    let log = Arc::try_unwrap(db)
        .expect("the repository is dropped")
        .into_transaction_log();
    assert_eq!(log.len(), 1, "one transaction: {log:?}");
    let sql = log[0].statements().to_vec();
    let delete = sql
        .iter()
        .find(|statement| statement.sql.contains("DELETE FROM"))
        .expect("the old chapters are deleted");
    assert_filters(delete, "file_chapters", "file_id", "=");
    assert_bound(delete, &file.to_string());
    let insert = sql
        .iter()
        .find(|statement| statement.sql.contains("INSERT INTO"))
        .expect("the new chapters are inserted");
    assert_contains(insert, r#""file_chapters""#);
    assert_bound(insert, "Chapter 1");
}

/// An empty set deletes and inserts nothing: no `INSERT ... VALUES ()`.
#[tokio::test]
async fn an_empty_replacement_only_deletes() {
    let db = connection(empty_mock());
    let repo = SqlChapterRepository::new(db.clone());
    repo.replace_for_file(Uuid::from_u128(0x266), Vec::new())
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    assert!(
        sql.iter()
            .all(|statement| !statement.sql.contains("INSERT")),
        "{sql:?}"
    );
    assert!(sql.iter().any(|statement| statement.sql.contains("DELETE")));
}

/// The read is the file's rows by index; a row holding a negative time,
/// which the table's `CHECK` refuses, is an error.
#[tokio::test]
async fn chapters_are_read_by_file_in_index_order_and_a_corrupt_row_is_an_error() {
    let file = Uuid::from_u128(0x267);
    let stored = |start_ms: i64| {
        row([
            ("file_id", Value::from(file)),
            ("chapter_index", Value::from(0_i32)),
            ("start_ms", Value::from(start_ms)),
            ("end_ms", Value::from(5_000_i64)),
            ("title", Value::from(Some("Opening".to_owned()))),
        ])
    };
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![stored(1_000)], vec![stored(-1)]]),
    );
    let repo = SqlChapterRepository::new(db.clone());

    assert_eq!(
        repo.find_by_file_id(file).await.unwrap(),
        vec![Chapter {
            index: 0,
            start_ms: 1_000,
            end_ms: 5_000,
            title: Some("Opening".into()),
        }]
    );
    assert!(repo.find_by_file_id(file).await.is_err());
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "file_chapters", "file_id", "=");
    assert_bound(&sql[0], &file.to_string());
    assert_contains(&sql[0], r#"ORDER BY "file_chapters"."chapter_index" ASC"#);
}
