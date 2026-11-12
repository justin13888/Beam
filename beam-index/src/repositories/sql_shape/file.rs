use super::*;
use beam_domain::repositories::FileRepository;

use crate::repositories::SqlFileRepository;
use std::path::Path;

#[tokio::test]
async fn lookups_filter_on_the_column_they_are_named_for() {
    let path = "/videos/a.mkv";
    let library = Uuid::from_u128(11);
    let entry = Uuid::from_u128(12);
    let episode = Uuid::from_u128(13);

    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo.find_by_path(path).await;
    let _ = repo.find_by_hash(0xdead_beef).await;
    let _ = repo.find_all_by_library(library).await;
    let _ = repo.find_by_movie_entry_id(entry).await;
    let _ = repo.find_by_episode_id(episode).await;
    let _ = repo.find_all_by_library_including_missing(library).await;
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "files", "file_path", "=");
    assert_bound(&sql[0], path);
    assert_filters(&sql[1], "files", "hash_xxh3", "=");
    assert_bound(&sql[1], &0xdead_beefi64.to_string());
    assert_filters(&sql[2], "files", "library_id", "=");
    assert_bound(&sql[2], &library.to_string());
    assert_filters(&sql[3], "files", "movie_entry_id", "=");
    assert_bound(&sql[3], &entry.to_string());
    assert_filters(&sql[4], "files", "episode_id", "=");
    assert_bound(&sql[4], &episode.to_string());
    assert_filters(&sql[5], "files", "library_id", "=");
    assert_bound(&sql[5], &library.to_string());
}

/// The files under a folder are one query, narrowed to the library and
/// to present rows, with the folder bound as a literal prefix ending in
/// a separator -- whole components only, and no `LIKE` pattern for a `_`
/// or `%` in a folder name to widen.
#[tokio::test]
async fn the_files_under_a_folder_are_one_literal_prefix_query() {
    let library = Uuid::from_u128(15);
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo
        .find_all_under(library, Path::new("/videos/Sho_w"))
        .await;
    let _ = repo
        .find_all_under(library, Path::new("/videos/Sho_w/"))
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 2);
    for statement in &sql {
        assert_filters(statement, "files", "library_id", "=");
        assert_bound(statement, &library.to_string());
        assert_filters(statement, "files", "missing_since", "IS NULL");
        assert_contains(statement, r#"starts_with("files"."file_path", "#);
        assert!(
            !statement.sql.contains("LIKE"),
            "no pattern: {}",
            statement.sql
        );
        let values = bound_values(statement);
        assert_eq!(
            values
                .iter()
                .filter(|v| v.contains("\"/videos/Sho_w/\""))
                .count(),
            1,
            "the folder, with one trailing separator: {values:?}"
        );
    }
}

/// The soft-delete split (issue #179): a visible read must exclude a
/// missing row, a reconcile read must not. Dropping the filter from a
/// visible read would put a file that is not on disk back in browse and
/// streaming; adding it to a reconcile read would make a returning path
/// index as a new file and orphan its playback progress.
#[tokio::test]
async fn visible_reads_exclude_missing_rows_and_reconcile_reads_include_them() {
    let id = Uuid::from_u128(14);
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results((0..6).map(|_| Vec::<Row>::new()))
            .append_query_results([vec![row([("num_items", Value::BigInt(Some(0)))])]])
            .append_query_results((0..2).map(|_| Vec::<Row>::new())),
    );
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo.find_by_id(id).await;
    let _ = repo.find_by_hash(1).await;
    let _ = repo.find_all_by_library(id).await;
    let _ = repo.find_by_movie_entry_id(id).await;
    let _ = repo.find_by_episode_id(id).await;
    let _ = repo.find_all_by_library_including_missing(id).await;
    let _ = repo.count_all().await;
    let _ = repo.find_by_path("/videos/a.mkv").await;
    drop(repo);

    let sql = statements(db);
    let (visible, reconcile): (Vec<usize>, Vec<usize>) = (vec![0, 1, 2, 3, 4, 6], vec![5, 7]);
    for i in visible {
        assert_filters(&sql[i], "files", "missing_since", "IS NULL");
    }
    for i in reconcile {
        // The column is always selected; what must be absent is a filter.
        assert!(
            !sql[i].sql.contains(r#""files"."missing_since" IS"#),
            "a reconcile read must see missing rows, got:\n{}",
            sql[i].sql
        );
    }
    assert_filters(&sql[0], "files", "id", "=");
    assert_bound(&sql[0], &id.to_string());
}

/// The relink lookup (issue #180) is scoped to one library and one hash,
/// and is a reconcile read: the row a moved file is matched to is
/// usually one already marked missing.
#[tokio::test]
async fn the_relink_lookup_binds_library_and_hash_and_sees_missing_rows() {
    let library = Uuid::from_u128(15);
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo
        .find_by_library_and_hash_including_missing(library, 0xfeed)
        .await;
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "files", "hash_xxh3", "=");
    assert_bound(&sql[0], &0xfeed_i64.to_string());
    assert_filters(&sql[0], "files", "library_id", "=");
    assert_bound(&sql[0], &library.to_string());
    assert!(
        !sql[0].sql.contains(r#""files"."missing_since" IS"#),
        "a reconcile read must see missing rows, got:\n{}",
        sql[0].sql
    );
}

/// A page of a library's files is a visible read scoped to the library
/// that seeks past its cursor on `(file_path, id)`, ascending, and
/// limits itself (issue #190).
#[tokio::test]
async fn a_page_of_a_library_s_files_seeks_by_path_then_id() {
    use beam_domain::models::file::LibraryFilePosition;

    let library = Uuid::from_u128(17);
    let after = Uuid::from_u128(18);
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo
        .find_page_by_library(
            library,
            Some(LibraryFilePosition {
                path: std::path::PathBuf::from("/lib/b.mkv"),
                id: after,
            }),
            std::num::NonZeroU32::new(101).unwrap(),
        )
        .await;
    drop(repo);

    let sql = statements(db);
    let page = &sql[0];
    assert_filters(page, "files", "library_id", "=");
    assert_bound(page, &library.to_string());
    assert_filters(page, "files", "missing_since", "IS NULL");
    assert_filters(page, "files", "file_path", ">");
    assert_filters(page, "files", "file_path", "=");
    assert_filters(page, "files", "id", ">");
    assert_bound(page, "/lib/b.mkv");
    assert_bound(page, &after.to_string());
    assert_contains(
        page,
        r#"ORDER BY "files"."file_path" ASC, "files"."id" ASC"#,
    );
    assert_bound(page, "101");
}

/// The rows beneath a directory are found by one prefix match scoped to
/// the library, with the directory's own wildcards escaped, and missing
/// rows are not filtered out.
#[tokio::test]
async fn the_rows_beneath_a_directory_are_one_escaped_prefix_match() {
    let library = Uuid::from_u128(16);
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let _ = repo
        .find_beneath_including_missing(library, std::path::Path::new("/lib/Show_%1/"))
        .await;
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "files", "library_id", "=");
    assert_bound(&sql[0], &library.to_string());
    assert_filters(&sql[0], "files", "file_path", "LIKE");
    // A debug string: each `\` of the pattern reads `\\`.
    assert_bound(&sql[0], r"/lib/Show\\_\\%1/%");
    assert!(
        sql[0].sql.contains("ESCAPE"),
        "the pattern names its escape character, got:\n{}",
        sql[0].sql
    );
    assert!(
        !sql[0].sql.contains(r#""files"."missing_since" IS"#),
        "a reconcile read must see missing rows, got:\n{}",
        sql[0].sql
    );
}

#[tokio::test]
async fn mark_missing_stamps_only_the_listed_rows_not_already_missing() {
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let a = Uuid::from_u128(21);
    let b = Uuid::from_u128(22);
    let _ = repo
        .mark_missing(
            vec![a, b],
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        )
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "marking is a single UPDATE");
    assert_contains(&sql[0], "UPDATE");
    assert_contains(&sql[0], r#"SET "missing_since" = $1"#);
    assert_filters(&sql[0], "files", "id", "IN");
    assert_filters(&sql[0], "files", "missing_since", "IS NULL");
    assert_bound(&sql[0], &a.to_string());
    assert_bound(&sql[0], &b.to_string());
    assert_bound(&sql[0], "2023-11-14");
}

#[tokio::test]
async fn restore_clears_the_stamp_on_exactly_one_row() {
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let id = Uuid::from_u128(23);
    let _ = repo.restore(id).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "UPDATE");
    assert_contains(&sql[0], r#"SET "missing_since" = $1"#);
    assert_filters(&sql[0], "files", "id", "=");
    assert_bound(&sql[0], &id.to_string());
    assert!(
        bound_values(&sql[0]).iter().any(|v| v.contains("None")),
        "restore must bind NULL for missing_since, got {:?}",
        bound_values(&sql[0])
    );
}

#[tokio::test]
async fn purge_missing_deletes_only_listed_rows_that_are_missing() {
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let a = Uuid::from_u128(24);
    let b = Uuid::from_u128(25);
    let _ = repo.purge_missing(vec![a, b]).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "DELETE FROM");
    assert_filters(&sql[0], "files", "id", "IN");
    assert_filters(&sql[0], "files", "missing_since", "IS NOT NULL");
    assert_bound(&sql[0], &a.to_string());
    assert_bound(&sql[0], &b.to_string());
}

#[tokio::test]
async fn empty_id_lists_issue_no_statement() {
    let db = connection(empty_mock());
    let repo = SqlFileRepository::new(db.clone());
    let marked = repo
        .mark_missing(Vec::new(), chrono::Utc::now())
        .await
        .unwrap();
    let purged = repo.purge_missing(Vec::new()).await.unwrap();
    drop(repo);

    assert_eq!((marked, purged), (0, 0));
    assert!(
        statements(db).is_empty(),
        "an empty id list must not reach the database -- `... IN ()` is \
         either a syntax error or, worse, matches every row"
    );
}
