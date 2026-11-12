use super::*;
use beam_domain::models::playback_telemetry::PlaybackTelemetryEvent;
use beam_domain::models::playback_telemetry::test_utils::{rebuffer_key, start_key, switch_key};
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
