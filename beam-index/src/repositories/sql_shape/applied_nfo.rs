use super::*;
use std::path::{Path, PathBuf};

use beam_domain::models::applied_nfo::RecordAppliedNfo;
use beam_domain::repositories::AppliedNfoRepository;

use crate::repositories::SqlAppliedNfoRepository;

/// The record is keyed by the path alone and rewrites what a re-read
/// learns, never the row's id or `created_at`; the read that follows is
/// by that same path (issue #184).
#[tokio::test]
async fn record_by_path_conflicts_on_the_path_and_keeps_id_and_created_at() {
    let db = connection(empty_mock());
    let repo = SqlAppliedNfoRepository::new(db.clone());
    let _ = repo
        .record_by_path(RecordAppliedNfo {
            library_id: Uuid::from_u128(2),
            path: PathBuf::from("/videos/Matrix/movie.nfo"),
            size_bytes: 10,
            content_hash: "c0ffee".to_string(),
            change_stamp: None,
        })
        .await;
    drop(repo);

    let sql = statements(db);
    let insert = &sql[0];
    assert_contains(insert, r#"ON CONFLICT ("path") DO UPDATE"#);
    for column in [
        "library_id",
        "size_bytes",
        "content_hash",
        "change_stamp",
        "updated_at",
    ] {
        assert_contains(insert, &format!(r#""{column}" = "excluded"."{column}""#));
    }
    for kept in ["id", "created_at", "path"] {
        assert!(
            !insert
                .sql
                .contains(&format!(r#""{kept}" = "excluded"."{kept}""#)),
            "{kept} is never rewritten: {}",
            insert.sql
        );
    }
    assert_bound(insert, "c0ffee");
    let read = &sql[1];
    assert_filters(read, "applied_nfos", "path", "=");
    assert_bound(read, "/videos/Matrix/movie.nfo");
}

/// Listing binds the library and orders by path; deleting nothing issues
/// nothing.
#[tokio::test]
async fn listing_filters_and_orders_and_an_empty_delete_is_no_statement() {
    let db = connection(empty_mock());
    let repo = SqlAppliedNfoRepository::new(db.clone());
    let _ = repo.find_all_by_library(Uuid::from_u128(4)).await;
    assert_eq!(repo.delete_by_ids(Vec::new()).await.unwrap(), 0);
    let _ = repo.delete_by_ids(vec![Uuid::from_u128(5)]).await;
    let _ = repo.find_by_path(Path::new("/videos/x.nfo")).await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 3, "the empty delete issued nothing");
    assert_filters(&sql[0], "applied_nfos", "library_id", "=");
    assert_bound(&sql[0], &Uuid::from_u128(4).to_string());
    assert_contains(&sql[0], r#"ORDER BY "applied_nfos"."path" ASC"#);
    assert!(sql[1].sql.starts_with("DELETE"), "{}", sql[1].sql);
    assert_filters(&sql[1], "applied_nfos", "id", "IN");
    assert_bound(&sql[1], &Uuid::from_u128(5).to_string());
    assert_filters(&sql[2], "applied_nfos", "path", "=");
}

/// The records beneath a directory go in one statement: a prefix match
/// scoped to the library, the directory's own wildcards escaped.
#[tokio::test]
async fn the_records_beneath_a_directory_are_one_escaped_prefix_delete() {
    let library = Uuid::from_u128(6);
    let db = connection(empty_mock());
    let repo = SqlAppliedNfoRepository::new(db.clone());
    let _ = repo
        .delete_beneath(library, Path::new("/lib/Show_%1/"))
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1);
    assert!(sql[0].sql.starts_with("DELETE"), "{}", sql[0].sql);
    assert_filters(&sql[0], "applied_nfos", "library_id", "=");
    assert_bound(&sql[0], &library.to_string());
    assert_filters(&sql[0], "applied_nfos", "path", "LIKE");
    // A debug string: each `\` of the pattern reads `\\`.
    assert_bound(&sql[0], r"/lib/Show\\_\\%1/%");
    assert!(
        sql[0].sql.contains("ESCAPE"),
        "the pattern names its escape character, got:\n{}",
        sql[0].sql
    );
}
