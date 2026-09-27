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

    /// The list reads join `files` on the progress row's file and keep only
    /// present files, in the same statement that carries the `LIMIT`/`OFFSET`
    /// or the `COUNT` -- so a missing file cannot take a slot in a page or in
    /// the total (issue #179).
    #[track_caller]
    fn assert_joins_present_files(statement: &Statement) {
        assert_contains(
            statement,
            r#"INNER JOIN "files" ON "playback_progress"."file_id" = "files"."id""#,
        );
        assert_contains(statement, r#""files"."missing_since" IS NULL"#);
    }

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
        assert_eq!(sql.len(), 1);
        assert_joins_present_files(&sql[0]);
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
        assert_eq!(sql.len(), 1);
        assert_joins_present_files(&sql[0]);
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
        assert_eq!(sql.len(), 1);
        assert_joins_present_files(&sql[0]);
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

mod library_shape {
    use super::*;
    use beam_domain::models::library_shape::{FilesByContentType, NamedCount};
    use beam_domain::repositories::LibraryShapeRepository;
    use beam_domain::utils::telemetry::{FileSizeBucket, UNKNOWN_LABEL};

    use crate::repositories::SqlLibraryShapeRepository;

    fn count(n: i64) -> Value {
        Value::BigInt(Some(n))
    }

    fn text(s: &str) -> Value {
        Value::String(Some(s.to_string()))
    }

    /// A store whose four aggregates answer with distinct numbers, so a
    /// column read into the wrong field shows up as a wrong number.
    fn answering_mock() -> MockDatabase {
        let mut files = vec![
            ("movie_files", count(3)),
            ("episode_files", count(5)),
            ("unclassified_files", count(7)),
            ("total_bytes", count(123_456)),
        ];
        // Bucket `i` holds `100 + i` files: every bucket distinct.
        for (i, bucket) in FileSizeBucket::ALL.into_iter().enumerate() {
            files.push((bucket.as_str(), count(100 + i as i64)));
        }
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![row([
                ("libraries", count(2)),
                ("movies", count(11)),
                ("shows", count(13)),
                ("seasons", count(17)),
                ("episodes", count(19)),
            ])]])
            .append_query_results([vec![row(files)]])
            .append_query_results([vec![
                row([("name", text(UNKNOWN_LABEL)), ("n", count(1))]),
                row([("name", text("avi")), ("n", count(4))]),
            ]])
            .append_query_results([vec![
                row([
                    ("stream_type", text("audio")),
                    ("name", text("aac")),
                    ("n", count(6)),
                ]),
                row([
                    ("stream_type", text("video")),
                    ("name", text("hevc")),
                    ("n", count(8)),
                ]),
                row([
                    ("stream_type", text("video")),
                    ("name", text("h264")),
                    ("n", count(9)),
                ]),
                row([
                    ("stream_type", text("subtitle")),
                    ("name", text("subrip")),
                    ("n", count(10)),
                ]),
            ]])
    }

    #[tokio::test]
    async fn every_aggregate_lands_in_its_own_field() {
        let db = connection(answering_mock());
        let repo = SqlLibraryShapeRepository::new(db.clone());

        let shape = repo.shape().await.unwrap();

        assert_eq!(
            (
                shape.libraries,
                shape.movies,
                shape.shows,
                shape.seasons,
                shape.episodes
            ),
            (2, 11, 13, 17, 19)
        );
        assert_eq!(
            shape.files,
            FilesByContentType {
                movie: 3,
                episode: 5,
                unclassified: 7,
            }
        );
        assert_eq!(shape.total_bytes, 123_456);
        for (i, (bucket, n)) in shape.file_sizes.iter().enumerate() {
            assert_eq!(n, 100 + i as u64, "{bucket:?}");
        }
        assert_eq!(
            shape.containers,
            vec![NamedCount::new("avi", 4), NamedCount::new(UNKNOWN_LABEL, 1)],
            "sorted by name whatever order the database answered in"
        );
        assert_eq!(
            shape.video_codecs,
            vec![NamedCount::new("h264", 9), NamedCount::new("hevc", 8)]
        );
        assert_eq!(shape.audio_codecs, vec![NamedCount::new("aac", 6)]);
        assert_eq!(shape.subtitle_codecs, vec![NamedCount::new("subrip", 10)]);
    }

    /// A soft-deleted file (issue #179) is in no count: every aggregate that
    /// reads `files` excludes it, and the container aggregate names a
    /// missing container with the report's `unknown` label.
    #[tokio::test]
    async fn every_file_aggregate_excludes_missing_files() {
        let db = connection(answering_mock());
        let repo = SqlLibraryShapeRepository::new(db.clone());
        repo.shape().await.unwrap();
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 4);
        for statement in &sql {
            let reads = statement.sql.matches(" files f ").count();
            let filters = statement.sql.matches("f.missing_since IS NULL").count();
            assert_eq!(
                reads, filters,
                "every read of files filters missing rows:\n{}",
                statement.sql
            );
        }
        assert_bound(&sql[2], UNKNOWN_LABEL);
        // The histogram's buckets are half-open and adjacent: each bound
        // opens one bucket and closes the one before it.
        for bucket in FileSizeBucket::ALL {
            let lower = bucket.lower_bound_bytes();
            assert_contains(&sql[1], &format!("f.file_size >= {lower}"));
            if let Some(upper) = bucket.upper_bound_bytes() {
                assert_contains(&sql[1], &format!("f.file_size < {upper}"));
            }
        }
    }

    #[tokio::test]
    async fn an_unknown_stream_type_is_an_error_not_a_silent_drop() {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres)
                .append_query_results([vec![row([
                    ("libraries", count(0)),
                    ("movies", count(0)),
                    ("shows", count(0)),
                    ("seasons", count(0)),
                    ("episodes", count(0)),
                ])]])
                .append_query_results([vec![row([
                    ("movie_files", count(0)),
                    ("episode_files", count(0)),
                    ("unclassified_files", count(0)),
                    ("total_bytes", count(0)),
                ]
                .into_iter()
                .chain(FileSizeBucket::ALL.map(|b| (b.as_str(), count(0)))))]])
                .append_query_results([Vec::<Row>::new()])
                .append_query_results([vec![row([
                    ("stream_type", text("data")),
                    ("name", text("bin_data")),
                    ("n", count(1)),
                ])]]),
        );
        let repo = SqlLibraryShapeRepository::new(db);

        assert!(repo.shape().await.is_err());
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

mod show {
    use super::*;
    use beam_domain::models::CreateEpisode;
    use beam_domain::repositories::ShowRepository;

    use crate::repositories::SqlShowRepository;

    /// Two statements whichever way the insert goes: the atomic insert, then
    /// the read-back of the row that won.
    #[tokio::test]
    async fn find_or_create_episode_inserts_with_do_nothing_on_the_pair_then_reads_the_pair() {
        let db = connection(empty_mock());
        let repo = SqlShowRepository::new(db.clone());
        let season = Uuid::from_u128(71);
        let _ = repo
            .find_or_create_episode(CreateEpisode {
                season_id: season,
                episode_number: 7,
                title: "Seven".to_string(),
                runtime: None,
            })
            .await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 2, "one insert, one read-back: {sql:?}");
        assert_contains(
            &sql[0],
            r#"ON CONFLICT ("season_id", "episode_number") DO NOTHING"#,
        );
        assert!(
            !sql[0].sql.contains("DO UPDATE"),
            "an existing episode must never be written to:\n{}",
            sql[0].sql
        );
        assert_filters(&sql[1], "episodes", "season_id", "=");
        assert_filters(&sql[1], "episodes", "episode_number", "=");
        assert_bound(&sql[1], &season.to_string());
        assert_bound(&sql[1], "7");
    }
}

/// Title identity and liveness (issue #183), for movies and shows alike.
mod title_identity {
    use super::*;
    use beam_domain::models::{CreateMovie, CreateShow, MovieSearchQuery, ShowSearchQuery};
    use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
    use beam_domain::repositories::{MovieRepository, ShowRepository};

    use crate::repositories::{SqlMovieRepository, SqlShowRepository};

    fn stored_movie() -> beam_entity::movie::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        beam_entity::movie::Model {
            id: Uuid::from_u128(81),
            title: "Amelie".to_string(),
            identity_key: Some("amelie|2001".to_string()),
            title_localized: None,
            description: None,
            year: Some(2001),
            release_date: None,
            runtime_mins: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            rating_imdb: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn stored_show() -> beam_entity::show::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        beam_entity::show::Model {
            id: Uuid::from_u128(82),
            title: "Shogun".to_string(),
            identity_key: Some("shogun|".to_string()),
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Find-or-create is the atomic insert on the identity key, then a read
    /// by that key -- never a lookup by display title, and never an update of
    /// the row that already holds the key.
    #[tokio::test]
    async fn find_or_create_inserts_with_do_nothing_on_the_identity_key_then_reads_by_it() {
        let db = connection(empty_mock());
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies
            .find_or_create_by_identity(CreateMovie::new("Amélie", Some(2001), None))
            .await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows
            .find_or_create_by_identity(CreateShow::new("Shogun", None))
            .await;
        drop((movies, shows));

        let sql = statements(db);
        assert_eq!(sql.len(), 4, "an insert and a read-back each: {sql:?}");
        for (insert, read, table, key) in [
            (&sql[0], &sql[1], "movies", "amelie|2001"),
            (&sql[2], &sql[3], "shows", "shogun|"),
        ] {
            assert_contains(insert, r#"ON CONFLICT ("identity_key") DO NOTHING"#);
            assert!(!insert.sql.contains("DO UPDATE"), "{}", insert.sql);
            assert_filters(read, table, "identity_key", "=");
            assert_bound(read, key);
            assert!(
                !read.sql.contains(r#""title" ="#),
                "a title is never looked up by its display title:\n{}",
                read.sql
            );
        }
    }

    /// Browse and search list only titles with a present file: the
    /// soft-delete filter sits inside the liveness join of every search.
    #[tokio::test]
    async fn search_keeps_only_titles_with_a_present_file() {
        let db = connection(empty_mock());
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies.search(&MovieSearchQuery::default()).await;
        let _ = movies
            .search(&MovieSearchQuery {
                query: Some("amelie".to_string()),
                year: Some(2001),
                ..Default::default()
            })
            .await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows.search(&ShowSearchQuery::default()).await;
        drop((movies, shows));

        let sql = statements(db);
        for statement in &sql[..2] {
            assert_contains(statement, "JOIN files f ON f.movie_entry_id = me.id");
            assert_contains(statement, "me.movie_id = movies.id");
            assert_contains(statement, "f.missing_since IS NULL");
        }
        assert_contains(&sql[2], "JOIN files f ON f.episode_id = e.id");
        assert_contains(&sql[2], "se.show_id = shows.id");
        assert_contains(&sql[2], "f.missing_since IS NULL");
        // The liveness condition binds nothing, so the query text stays `$1`
        // and ORDER BY's reuse of it still ranks by the search text.
        assert_bound(&sql[1], "amelie");
        assert_contains(&sql[1], "similarity(title, $1)");
    }

    /// Enrichment writes display fields only. An `UPDATE` that set
    /// `identity_key` -- even to the value it had -- is the bug this issue
    /// removed, waiting to come back.
    #[tokio::test]
    async fn apply_enrichment_never_writes_the_identity_key() {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres)
                .append_query_results([vec![stored_movie()], vec![stored_movie()]])
                .append_query_results([vec![stored_show()], vec![stored_show()]])
                .append_exec_results((0..4).map(|_| sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                })),
        );
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies
            .apply_enrichment(
                Uuid::from_u128(81),
                &MovieEnrichment {
                    title: "Amélie".to_string(),
                    ..Default::default()
                },
            )
            .await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows
            .apply_enrichment(
                Uuid::from_u128(82),
                &ShowEnrichment {
                    title: "Shōgun".to_string(),
                    ..Default::default()
                },
            )
            .await;
        drop((movies, shows));

        let updates: Vec<Statement> = statements(db)
            .into_iter()
            .filter(|s| s.sql.starts_with("UPDATE"))
            .collect();
        assert_eq!(updates.len(), 2, "one update per title: {updates:?}");
        for update in &updates {
            assert_contains(update, r#""title" ="#);
            // Only the `SET` list: `RETURNING` names every column.
            let set_list = update.sql.split(" WHERE ").next().unwrap_or_default();
            assert!(
                !set_list.contains("identity_key"),
                "enrichment must not write the identity key:\n{}",
                update.sql
            );
        }
    }

    /// The backfill only ever keys a keyless row, and only with a key no
    /// other row holds.
    #[tokio::test]
    async fn assign_identity_key_touches_only_a_keyless_row_and_a_free_key() {
        let db = connection(empty_mock());
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies
            .assign_identity_key(Uuid::from_u128(83), "dune|1984")
            .await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows
            .assign_identity_key(Uuid::from_u128(84), "shogun|")
            .await;
        drop((movies, shows));

        let sql = statements(db);
        for (statement, table, id, key) in [
            (&sql[0], "movies", Uuid::from_u128(83), "dune|1984"),
            (&sql[1], "shows", Uuid::from_u128(84), "shogun|"),
        ] {
            assert!(statement.sql.starts_with("UPDATE"), "{}", statement.sql);
            assert_filters(statement, table, "id", "=");
            assert_bound(statement, &id.to_string());
            assert_filters(statement, table, "identity_key", "IS NULL");
            assert_contains(statement, "NOT EXISTS");
            assert_bound(statement, key);
        }
    }

    /// The backfill reads keyless titles oldest first, so of two legacy
    /// duplicates the original -- not whichever the planner returns first --
    /// takes the key.
    #[tokio::test]
    async fn find_unkeyed_reads_keyless_titles_oldest_first() {
        let db = connection(empty_mock());
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies.find_unkeyed().await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows.find_unkeyed().await;
        drop((movies, shows));

        let sql = statements(db);
        for (statement, table) in [(&sql[0], "movies"), (&sql[1], "shows")] {
            assert_filters(statement, table, "identity_key", "IS NULL");
            assert_contains(
                statement,
                &format!(r#"ORDER BY "{table}"."created_at" ASC, "{table}"."id" ASC"#),
            );
        }
    }

    /// Orphan deletion walks down from the file rows -- entries (episodes)
    /// before titles -- counts any file row, soft-deleted or not, and binds
    /// the caller's cutoff to every step that has a `created_at`.
    #[tokio::test]
    async fn delete_orphaned_deletes_children_first_and_binds_the_cutoff() {
        let cutoff = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let db = connection(empty_mock());
        let movies = SqlMovieRepository::new(db.clone());
        let _ = movies.delete_orphaned(cutoff).await;
        let shows = SqlShowRepository::new(db.clone());
        let _ = shows.delete_orphaned(cutoff).await;
        drop((movies, shows));

        let sql = statements(db);
        let tables: Vec<&str> = sql
            .iter()
            .map(|s| {
                s.sql
                    .strip_prefix("DELETE FROM ")
                    .and_then(|rest| rest.split_whitespace().next())
                    .unwrap_or("")
            })
            .collect();
        assert_eq!(
            tables,
            vec!["movie_entries", "movies", "episodes", "seasons", "shows"],
            "children first, so a title is orphaned once its last child goes"
        );
        for statement in sql
            .iter()
            .filter(|s| !s.sql.starts_with("DELETE FROM seasons"))
        {
            assert_contains(statement, "created_at < $1");
            assert_bound(statement, "2023-11-14");
        }
        for statement in [&sql[0], &sql[2]] {
            assert!(
                !statement.sql.contains("missing_since"),
                "a soft-deleted file still holds its title:\n{}",
                statement.sql
            );
        }
    }
}

mod playback_telemetry {
    use super::*;
    use beam_domain::models::playback_telemetry::PlaybackTelemetryEvent;
    use beam_domain::models::playback_telemetry::test_utils::{
        rebuffer_key, start_key, switch_key,
    };
    use beam_domain::repositories::PlaybackTelemetryRepository;
    use beam_entity::{playback_rebuffer_count, playback_start_count, playback_switch_count};
    use chrono::NaiveDate;
    use sea_orm::DbErr;
    use sea_orm::{EntityName, EntityTrait, IdenStatic, Iterable, PrimaryKeyToColumn};

    use crate::repositories::SqlPlaybackTelemetryRepository;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    /// The conflict target is the table's whole primary key -- read from the
    /// entity rather than restated, so a key column added to the table and
    /// forgotten in the upsert fails here. A target missing a key column has
    /// no unique index behind it, and Postgres refuses the statement.
    #[track_caller]
    fn assert_conflicts_on_the_primary_key<E: EntityTrait>(statement: &Statement) {
        let columns: Vec<String> = E::PrimaryKey::iter()
            .map(|key| format!(r#""{}""#, key.into_column().as_str()))
            .collect();
        assert_contains(
            statement,
            &format!("ON CONFLICT ({}) DO UPDATE", columns.join(", ")),
        );
    }

    /// Every transaction the repository ran, each as its statements in
    /// order -- `BEGIN` and `COMMIT`/`ROLLBACK` included.
    fn transactions(db: Arc<DatabaseConnection>) -> Vec<Vec<Statement>> {
        Arc::try_unwrap(db)
            .expect("drop the repository before draining its statement log")
            .into_transaction_log()
            .into_iter()
            .map(|transaction| transaction.statements().to_vec())
            .collect()
    }

    /// A value as the bound-parameter list spells it.
    fn bound(value: i64) -> String {
        format!("{:?}", sea_orm::Value::from(value))
    }

    /// One of each kind, and the start twice.
    fn mixed_batch() -> Vec<PlaybackTelemetryEvent> {
        vec![
            PlaybackTelemetryEvent::Start(start_key()),
            PlaybackTelemetryEvent::Rebuffer {
                key: rebuffer_key(),
                duration_ms: 1_234,
            },
            PlaybackTelemetryEvent::Switch(switch_key()),
            PlaybackTelemetryEvent::Start(start_key()),
        ]
    }

    /// A batch is one transaction holding one upsert per table, never a
    /// statement per event: a key named twice is bound once, as a count of
    /// two, and the conflict target is each table's whole primary key.
    #[tokio::test]
    async fn a_batch_is_one_transaction_with_one_upsert_per_table() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        repo.record_batch(day(), &mixed_batch()).await.unwrap();
        drop(repo);

        let log = transactions(db);
        assert_eq!(log.len(), 1, "the whole batch is one transaction");
        let sql = &log[0];
        assert_eq!(sql.len(), 5, "BEGIN, one upsert per table, COMMIT");
        assert_eq!(sql[0].sql, "BEGIN");
        assert_eq!(sql[4].sql, "COMMIT");
        assert_conflicts_on_the_primary_key::<playback_start_count::Entity>(&sql[1]);
        assert_conflicts_on_the_primary_key::<playback_rebuffer_count::Entity>(&sql[2]);
        assert_conflicts_on_the_primary_key::<playback_switch_count::Entity>(&sql[3]);
        for statement in &sql[1..4] {
            assert_bound(statement, "2026-09-27");
        }
        assert!(
            bound_values(&sql[1]).contains(&bound(2)),
            "the start named twice is one row counting two: {:?}",
            bound_values(&sql[1])
        );
    }

    /// On conflict every counter grows by what the batch brought -- the
    /// events, the total and every bucket, since a batch's rebuffers can fall
    /// into any of them -- rather than being overwritten by it.
    #[tokio::test]
    async fn every_counter_adds_the_batch_to_what_the_row_holds() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        repo.record_batch(day(), &mixed_batch()).await.unwrap();
        drop(repo);

        let log = transactions(db);
        let sql = &log[0];
        let counters: [(&str, Vec<&str>); 3] = [
            (playback_start_count::Entity.table_name(), vec!["count"]),
            (
                playback_rebuffer_count::Entity.table_name(),
                vec![
                    "events", "total_ms", "lt_1s", "s1_3", "s3_10", "s10_30", "ge_30s",
                ],
            ),
            (playback_switch_count::Entity.table_name(), vec!["count"]),
        ];
        for (statement, (table, columns)) in sql[1..4].iter().zip(counters) {
            for column in columns {
                assert_contains(
                    statement,
                    &format!(r#""{column}" = "{table}"."{column}" + "excluded"."{column}""#),
                );
            }
        }
        assert_bound(&sql[2], "1234");
    }

    /// A statement failing part-way rolls back what the batch already wrote,
    /// and nothing is committed: a failed batch is counted not at all.
    #[tokio::test]
    async fn a_failure_part_way_rolls_the_whole_batch_back() {
        let mock = MockDatabase::new(DbBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_exec_errors([DbErr::Custom("connection reset".to_string())]);
        let db = connection(mock);
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        let result = repo.record_batch(day(), &mixed_batch()).await;
        drop(repo);

        assert!(result.is_err());
        let log = transactions(db);
        assert_eq!(log.len(), 1);
        let sql: Vec<&str> = log[0].iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(sql.first(), Some(&"BEGIN"));
        assert_eq!(sql.last(), Some(&"ROLLBACK"));
        assert!(!sql.contains(&"COMMIT"), "{sql:?}");
        assert_eq!(
            sql.len(),
            4,
            "BEGIN, the upsert that ran, the one that failed, ROLLBACK"
        );
    }

    #[tokio::test]
    async fn an_empty_batch_touches_nothing() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        repo.record_batch(day(), &[]).await.unwrap();
        drop(repo);

        assert!(transactions(db).is_empty());
    }

    /// Pruning deletes from every table, strictly before the cutoff it is
    /// given -- never the cutoff day itself.
    #[tokio::test]
    async fn pruning_deletes_strictly_earlier_days_from_every_table() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        let _ = repo.prune_before(day()).await;
        drop(repo);

        let sql = statements(db);
        let tables: Vec<&str> = vec![
            playback_start_count::Entity.table_name(),
            playback_rebuffer_count::Entity.table_name(),
            playback_switch_count::Entity.table_name(),
        ];
        assert_eq!(sql.len(), tables.len());
        for (statement, table) in sql.iter().zip(tables) {
            assert_contains(statement, &format!(r#"DELETE FROM "{table}""#));
            assert_filters(statement, table, "day", "<");
            assert_bound(statement, "2026-09-27");
        }
    }

    /// Each read sums one table over the range it is given, both ends bound.
    #[tokio::test]
    async fn summarizing_reads_each_table_over_the_bound_range() {
        let db = connection(empty_mock());
        let repo = SqlPlaybackTelemetryRepository::new(db.clone());
        let from = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let _ = repo.summarize(from, day()).await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 3);
        for (statement, table) in sql.iter().zip([
            playback_start_count::Entity.table_name(),
            playback_rebuffer_count::Entity.table_name(),
            playback_switch_count::Entity.table_name(),
        ]) {
            assert_contains(statement, &format!("FROM {table} "));
            // The lower bound is the first parameter and the upper the second,
            // and each is bound to the end of the range it bounds.
            assert_contains(statement, "day >= $1");
            assert_contains(statement, "day <= $2");
            let values = bound_values(statement);
            assert_eq!(values.len(), 2, "{values:?}");
            assert!(values[0].contains("2026-08-01"), "{values:?}");
            assert!(values[1].contains("2026-09-27"), "{values:?}");
            assert_contains(statement, "GROUP BY");
        }
    }
}
