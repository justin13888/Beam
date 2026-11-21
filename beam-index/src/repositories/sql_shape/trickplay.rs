use super::*;

use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
use beam_domain::repositories::TrickplayRepository;

use crate::repositories::SqlTrickplayRepository;

/// A record upserts on its file and writes every column it owns, so a
/// later record replaces the earlier whole (issue #265).
#[tokio::test]
async fn a_record_upserts_every_column_on_the_file() {
    let file = Uuid::from_u128(0x268);
    let db = connection(empty_mock());
    let repo = SqlTrickplayRepository::new(db.clone());
    repo.upsert(TrickplayRecord {
        file_id: file,
        file_hash: u64::MAX,
        layout: None,
        generated_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    })
    .await
    .unwrap();
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert_contains(&sql[0], r#"ON CONFLICT ("file_id") DO UPDATE SET"#);
    for column in [
        "file_hash",
        "interval_ms",
        "tile_width",
        "tile_height",
        "columns",
        "rows",
        "tile_count",
        "generated_at",
    ] {
        assert_contains(&sql[0], &format!(r#""{column}" = "excluded"."{column}""#));
    }
    // A `u64` hash is stored as its bits.
    assert_bound(&sql[0], "-1");
}

/// A row reads back whole -- the hash's bits a `u64` again -- and one
/// whose layout is half there, which the table's `CHECK` refuses, is an
/// error.
#[tokio::test]
async fn a_stored_row_reads_back_and_a_partial_layout_is_an_error() {
    let file = Uuid::from_u128(0x269);
    let generated_at = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:00:00Z").unwrap();
    let stored = |tile_count: Option<i32>| {
        row([
            ("file_id", Value::from(file)),
            ("file_hash", Value::from(-2_i64)),
            ("interval_ms", Value::from(Some(10_000_i32))),
            ("tile_width", Value::from(Some(320_i32))),
            ("tile_height", Value::from(Some(180_i32))),
            ("columns", Value::from(Some(10_i32))),
            ("rows", Value::from(Some(10_i32))),
            ("tile_count", Value::from(tile_count)),
            ("generated_at", Value::from(generated_at)),
        ])
    };
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![stored(Some(720))], vec![stored(None)]]),
    );
    let repo = SqlTrickplayRepository::new(db.clone());

    assert_eq!(
        repo.find(file).await.unwrap(),
        Some(TrickplayRecord {
            file_id: file,
            file_hash: u64::MAX - 1,
            layout: Some(TrickplayLayout {
                interval_ms: 10_000,
                tile_width: 320,
                tile_height: 180,
                columns: 10,
                rows: 10,
                tile_count: 720,
            }),
            generated_at: generated_at.into(),
        })
    );
    assert!(repo.find(file).await.is_err());
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "file_trickplay", "file_id", "=");
    assert_bound(&sql[0], &file.to_string());
}

/// Records are found for exactly the ids asked about, and an empty list
/// asks nothing.
#[tokio::test]
async fn records_are_found_by_file_ids_and_none_are_asked_for_no_ids() {
    let ids = [Uuid::from_u128(0x26a), Uuid::from_u128(0x26b)];
    let db = connection(empty_mock());
    let repo = SqlTrickplayRepository::new(db.clone());
    assert!(repo.find_by_file_ids(&[]).await.unwrap().is_empty());
    let _ = repo.find_by_file_ids(&ids).await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert_filters(&sql[0], "file_trickplay", "file_id", "IN");
    for id in ids {
        assert_bound(&sql[0], &id.to_string());
    }
}
