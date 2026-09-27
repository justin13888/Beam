//! Hermetic assertions on the SQL these repositories generate.
//!
//! Everything below `RepositoryTrait` is a query builder, and a query builder
//! fails silently: drop a `WHERE`, flip an `ORDER BY`, or lose an `ON CONFLICT`
//! target and the code still compiles, the fakes still pass, and the behaviour
//! is only wrong against a real database. The `pg-integration` tier catches
//! that -- but it is opt-in and needs Postgres, so it cannot be the only guard.
//!
//! `sea_orm::MockDatabase` closes the gap: it records the statement before it
//! looks for a result, so a call can be driven with an empty result buffer and
//! the generated SQL inspected regardless of whether the call then succeeds.
//!
//! These assertions deliberately test *properties*, not the full statement
//! string. A test that pins the entire generated SQL is a second copy of the
//! query builder's output -- it fails on every harmless formatting change and
//! catches nothing a property does not. What is asserted here is what silently
//! breaks: which column a filter binds, which value it binds, the sort
//! direction, the pagination numbers, and the conflict target.

use std::collections::BTreeMap;
use std::sync::Arc;

use sea_orm::{DatabaseConnection, DbBackend, MockDatabase, Statement, Value};
use uuid::Uuid;

/// A row for [`MockDatabase::append_query_results`], built column by column.
pub type Row = BTreeMap<String, Value>;

fn row(columns: impl IntoIterator<Item = (&'static str, Value)>) -> Row {
    columns
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
}

/// A Postgres mock that answers every query with no rows and every write with
/// "one row affected", enough times that a multi-statement method runs to the
/// end instead of short-circuiting on the first missing result.
fn empty_mock() -> MockDatabase {
    let no_rows: Vec<Vec<Row>> = (0..12).map(|_| Vec::new()).collect();
    MockDatabase::new(DbBackend::Postgres)
        .append_query_results(no_rows)
        .append_exec_results((0..12).map(|_| sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }))
}

/// The connection a repository under test is built on.
fn connection(mock: MockDatabase) -> Arc<DatabaseConnection> {
    Arc::new(mock.into_connection())
}

/// Every statement the repository issued, in order.
///
/// Takes the connection by value: draining the log consumes the mock, so the
/// repository holding the other `Arc` handle must be dropped first.
fn statements(db: Arc<DatabaseConnection>) -> Vec<Statement> {
    Arc::try_unwrap(db)
        .expect("drop the repository before draining its statement log")
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect()
}

/// Assert `sql` binds `needle` -- a fragment such as `"user_id" = $1` -- with
/// the double quotes Postgres identifiers carry, so a match cannot come from a
/// column of the same name on another table.
#[track_caller]
fn assert_filters(statement: &Statement, table: &str, column: &str, operator: &str) {
    let needle = format!(r#""{table}"."{column}" {operator}"#);
    assert!(
        statement.sql.contains(&needle),
        "expected the statement to filter on `{needle}`, got:\n{}",
        statement.sql
    );
}

#[track_caller]
fn assert_contains(statement: &Statement, needle: &str) {
    assert!(
        statement.sql.contains(needle),
        "expected the statement to contain `{needle}`, got:\n{}",
        statement.sql
    );
}

/// The parameter values bound to `statement`, as debug strings -- enough to
/// assert *which* identifier a filter was given without depending on sea-query's
/// `Value` variants.
fn bound_values(statement: &Statement) -> Vec<String> {
    statement
        .values
        .as_ref()
        .map(|values| values.0.iter().map(|v| format!("{v:?}")).collect())
        .unwrap_or_default()
}

#[track_caller]
fn assert_bound(statement: &Statement, expected: &str) {
    let values = bound_values(statement);
    assert!(
        values.iter().any(|v| v.contains(expected)),
        "expected `{expected}` among the bound parameters {values:?} of:\n{}",
        statement.sql
    );
}

mod playback_progress {
    use super::*;
    use beam_domain::models::playback_progress::UpsertPlaybackProgress;
    use beam_domain::repositories::PlaybackProgressRepository;

    use crate::repositories::SqlPlaybackProgressRepository;

    #[tokio::test]
    async fn upsert_targets_the_user_file_unique_index_and_updates_the_mutable_columns() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackProgressRepository::new(db.clone());
        let _ = repo
            .upsert(UpsertPlaybackProgress {
                user_id: Uuid::nil(),
                file_id: Uuid::nil(),
                position_secs: 12.0,
                duration_secs: Some(100.0),
            })
            .await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 1, "the upsert must be a single statement");
        assert_contains(&sql[0], r#"ON CONFLICT ("user_id", "file_id") DO UPDATE"#);
        for column in ["position_secs", "duration_secs", "completed", "updated_at"] {
            assert_contains(&sql[0], &format!(r#""{column}" = "excluded"."{column}""#));
        }
        assert!(
            !sql[0].sql.contains(r#""id" = "excluded"."id""#),
            "the primary key must survive the conflict, not be overwritten:\n{}",
            sql[0].sql
        );
    }

    #[tokio::test]
    async fn find_by_user_and_file_filters_on_both_columns() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackProgressRepository::new(db.clone());
        let user = Uuid::from_u128(1);
        let file = Uuid::from_u128(2);
        let _ = repo.find_by_user_and_file(user, file).await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "playback_progress", "user_id", "=");
        assert_filters(&sql[0], "playback_progress", "file_id", "=");
        assert_bound(&sql[0], &user.to_string());
        assert_bound(&sql[0], &file.to_string());
    }

    #[tokio::test]
    async fn find_in_progress_excludes_completed_orders_desc_and_limits() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackProgressRepository::new(db.clone());
        let _ = repo.find_in_progress_by_user(Uuid::from_u128(7), 5).await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "playback_progress", "user_id", "=");
        assert_filters(&sql[0], "playback_progress", "completed", "=");
        assert_contains(&sql[0], r#"ORDER BY "playback_progress"."updated_at" DESC"#);
        assert_contains(&sql[0], "LIMIT $3");
        assert_bound(&sql[0], "5");
    }

    #[tokio::test]
    async fn find_page_keeps_completed_rows_and_binds_limit_and_offset() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackProgressRepository::new(db.clone());
        let _ = repo.find_page_by_user(Uuid::from_u128(7), 25, 50).await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "playback_progress", "user_id", "=");
        assert!(
            !sql[0].sql.contains(r#""playback_progress"."completed" ="#),
            "history includes completed rows, so it must not filter on `completed`:\n{}",
            sql[0].sql
        );
        assert_contains(&sql[0], r#"ORDER BY "playback_progress"."updated_at" DESC"#);
        assert_bound(&sql[0], "25");
        assert_bound(&sql[0], "50");
    }

    #[tokio::test]
    async fn count_by_user_is_scoped_to_the_user() {
        let db = connection(connection_with_count(3));
        let repo = SqlPlaybackProgressRepository::new(db.clone());
        let user = Uuid::from_u128(9);
        assert_eq!(repo.count_by_user(user).await.unwrap(), 3);
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "playback_progress", "user_id", "=");
        assert_bound(&sql[0], &user.to_string());
    }

    /// A mock whose single query result is a `COUNT(*)` of `n`.
    fn connection_with_count(n: i64) -> MockDatabase {
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![row([("num_items", Value::BigInt(Some(n)))])]])
    }
}

mod file {
    use super::*;
    use beam_domain::repositories::FileRepository;

    use crate::repositories::SqlFileRepository;

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
}

mod admin_log {
    use super::*;
    use beam_domain::repositories::AdminLogRepository;

    use crate::repositories::SqlAdminLogRepository;

    #[tokio::test]
    async fn list_orders_newest_first_and_paginates() {
        let db = connection(empty_mock());
        let repo = SqlAdminLogRepository::new(db.clone());
        let _ = repo.list(20, 40).await;
        drop(repo);

        let sql = statements(db);
        assert_contains(&sql[0], r#"ORDER BY "admin_logs"."created_at" DESC"#);
        assert_bound(&sql[0], "20");
        assert_bound(&sql[0], "40");
    }

    #[tokio::test]
    async fn list_by_category_adds_the_category_filter_to_the_same_ordering() {
        let db = connection(empty_mock());
        let repo = SqlAdminLogRepository::new(db.clone());
        let _ = repo
            .list_by_category(
                beam_domain::models::admin_log::AdminLogCategory::LibraryScan,
                10,
                0,
            )
            .await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "admin_logs", "category", "=");
        assert_contains(&sql[0], r#"ORDER BY "admin_logs"."created_at" DESC"#);
    }
}

mod library {
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
}

mod enrichment {
    use super::*;
    use beam_domain::repositories::EnrichmentStateRepository;

    use crate::repositories::SqlEnrichmentStateRepository;

    #[tokio::test]
    async fn fetch_due_takes_pending_rows_that_are_due_oldest_first() {
        let db = connection(empty_mock());
        let repo = SqlEnrichmentStateRepository::new(db.clone());
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let _ = repo.fetch_due(now, 25).await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "metadata_enrichment", "status", "=");
        assert_filters(&sql[0], "metadata_enrichment", "next_attempt_at", "IS NULL");
        assert_filters(&sql[0], "metadata_enrichment", "next_attempt_at", "<=");
        assert_contains(&sql[0], " OR ");
        assert_contains(
            &sql[0],
            r#"ORDER BY "metadata_enrichment"."next_attempt_at" ASC"#,
        );
        assert_bound(&sql[0], "25");
    }

    #[tokio::test]
    async fn mark_failed_on_a_row_that_no_longer_exists_is_a_no_op_not_an_error() {
        let db = connection(empty_mock());
        let repo = SqlEnrichmentStateRepository::new(db.clone());
        let id = Uuid::from_u128(51);
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        repo.mark_failed(id, "provider exploded", now)
            .await
            .expect("a vanished enrichment row is not an error to mark");
        drop(repo);

        let sql = statements(db);
        assert_eq!(
            sql.len(),
            1,
            "with no row to update, only the lookup is issued: {sql:?}"
        );
        assert_filters(&sql[0], "metadata_enrichment", "id", "=");
        assert_bound(&sql[0], &id.to_string());
    }
}

mod stream {
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
}
