use super::*;
use beam_domain::repositories::LibraryRepository;

use crate::repositories::SqlLibraryRepository;

#[tokio::test]
async fn count_files_counts_the_files_table_scoped_to_one_library() {
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![row([("num_items", Value::BigInt(Some(4)))])]]),
    );
    let repo = SqlLibraryRepository::new(db.clone());
    let library = Uuid::from_u128(41);
    assert_eq!(repo.count_files(library).await.unwrap(), 4);
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], r#"FROM "files""#);
    assert_filters(&sql[0], "files", "library_id", "=");
    assert_bound(&sql[0], &library.to_string());
    // A missing file (issue #179) is not part of the library a scan
    // reports.
    assert_filters(&sql[0], "files", "missing_since", "IS NULL");
}

#[tokio::test]
async fn delete_is_scoped_to_the_requested_library() {
    let db = connection(empty_mock());
    let repo = SqlLibraryRepository::new(db.clone());
    let library = Uuid::from_u128(42);
    let _ = repo.delete(library).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "DELETE FROM");
    assert_filters(&sql[0], "libraries", "id", "=");
    assert_bound(&sql[0], &library.to_string());
}
