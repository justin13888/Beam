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

#[path = "sql_shape/watch_state.rs"]
mod watch_state;

#[path = "sql_shape/file.rs"]
mod file;

#[path = "sql_shape/admin_log.rs"]
mod admin_log;

#[path = "sql_shape/library.rs"]
mod library;

#[path = "sql_shape/enrichment.rs"]
mod enrichment;

#[path = "sql_shape/library_shape.rs"]
mod library_shape;

#[path = "sql_shape/stream.rs"]
mod stream;

#[path = "sql_shape/show.rs"]
mod show;

#[path = "sql_shape/movie_entry.rs"]
mod movie_entry;

/// Title identity (issue #183), for movies and shows alike. Liveness is the
/// catalogue's: see `catalog_tests`.
#[path = "sql_shape/title_identity.rs"]
mod title_identity;

#[path = "sql_shape/playback_telemetry.rs"]
mod playback_telemetry;

#[path = "sql_shape/sidecar_subtitle.rs"]
mod sidecar_subtitle;

#[path = "sql_shape/access_policy.rs"]
mod access_policy;

#[path = "sql_shape/applied_nfo.rs"]
mod applied_nfo;

#[path = "sql_shape/language_preferences.rs"]
mod language_preferences;

#[path = "sql_shape/chapter.rs"]
mod chapter;

#[path = "sql_shape/trickplay.rs"]
mod trickplay;

/// The listings issue #328 restricts bind a policy's values among their own,
/// so each placeholder must still name the value it is compared with: a
/// numbering off by one compares a library id with a cursor's timestamp and
/// fails only against a real database.
#[path = "sql_shape/access_listings.rs"]
mod access_listings;

/// [`super::beneath_pattern`] read as Postgres reads a `LIKE .. ESCAPE '\'`
/// pattern: the escaped pattern matches exactly the paths strictly beneath
/// the directory, whatever wildcard or escape characters its names hold.
#[cfg(unix)]
#[path = "sql_shape/beneath_pattern_properties.rs"]
mod beneath_pattern_properties;
