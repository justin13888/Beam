use super::*;
use beam_domain::repositories::EnrichmentStateRepository;

use crate::repositories::SqlEnrichmentStateRepository;
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::services::{SequentialIdGenerator, TestClock};

/// A repository whose clock reads 2023-11-14 -- a day the wall clock
/// never will again -- and whose ids are sequential.
fn seamed(db: &Arc<DatabaseConnection>) -> SqlEnrichmentStateRepository {
    SqlEnrichmentStateRepository::with_seams(
        db.clone(),
        Arc::new(TestClock::starting_at(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        )),
        Arc::new(SequentialIdGenerator::new()),
    )
}

/// A new row's id and timestamps come from the injected seams, not the
/// wall clock and a random UUID.
#[tokio::test]
async fn a_new_row_is_stamped_from_the_injected_clock_and_ids() {
    let db = connection(empty_mock());
    let repo = seamed(&db);
    // The mock returns no inserted row; only the statement matters here.
    let _ = repo
        .ensure_pending(EnrichmentTargetId::Movie(Uuid::from_u128(7)))
        .await;
    drop(repo);

    let sql = statements(db);
    let insert = sql
        .iter()
        .find(|statement| statement.sql.starts_with("INSERT"))
        .expect("a missing row is inserted");
    assert_bound(insert, &SequentialIdGenerator::nth(0).to_string());
    assert_bound(insert, "2023-11-14");
}

/// Queueing every row for another pass stamps it as updated at the
/// injected clock's instant.
#[tokio::test]
async fn queueing_every_row_stamps_the_injected_clock() {
    let db = connection(empty_mock());
    let repo = seamed(&db);
    let _ = repo.request_refresh_all(false).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "UPDATE");
    assert_bound(&sql[0], "2023-11-14");
}

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
    // Never-attempted rows first: Postgres would otherwise put the NULLs
    // last and diverge from the in-memory double under a limit.
    assert_contains(
        &sql[0],
        r#"ORDER BY "metadata_enrichment"."next_attempt_at" ASC NULLS FIRST, "metadata_enrichment"."id" ASC"#,
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

/// A retry is stamped at the `now` its caller passes -- the service's
/// injected clock -- not at the database host's wall clock, and schedules
/// the attempt it was given.
#[tokio::test]
async fn a_retry_binds_the_callers_now_and_next_attempt() {
    let created: chrono::DateTime<chrono::FixedOffset> =
        chrono::DateTime::from_timestamp(1_600_000_000, 0)
            .unwrap()
            .into();
    let stored = beam_entity::metadata_enrichment::Model {
        id: Uuid::from_u128(52),
        movie_id: Some(Uuid::from_u128(53)),
        show_id: None,
        status: beam_entity::metadata_enrichment::EnrichmentStatus::Pending,
        attempts: 1,
        next_attempt_at: None,
        enriched_at: None,
        match_confidence: None,
        matched_ref: None,
        force_refresh: false,
        last_error: None,
        locked_fields: Vec::new(),
        created_at: created,
        updated_at: created,
    };
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![stored.clone()], vec![stored]]),
    );
    let repo = SqlEnrichmentStateRepository::new(db.clone());
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let next = chrono::DateTime::from_timestamp(1_700_000_600, 0).unwrap();
    repo.mark_retrying(Uuid::from_u128(52), "busy", 2, next, now)
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    let update = sql
        .iter()
        .find(|statement| statement.sql.starts_with("UPDATE"))
        .expect("the row is updated");
    assert_contains(update, r#""updated_at" = "#);
    assert_bound(update, "2023-11-14T22:13:20");
    assert_bound(update, "2023-11-14T22:23:20");
    assert_bound(update, "busy");
}

/// Refreshing everything is one `UPDATE` with no `WHERE` -- not a read of
/// every row and a write per row, as it was -- and a rematch clears the
/// match in the same statement.
#[tokio::test]
async fn refreshing_all_is_one_update_of_every_row() {
    let db = connection(empty_mock());
    let repo = SqlEnrichmentStateRepository::new(db.clone());
    let _ = repo.request_refresh_all(true).await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert!(sql[0].sql.starts_with("UPDATE"), "{}", sql[0].sql);
    assert!(!sql[0].sql.contains("WHERE"), "{}", sql[0].sql);
    assert_contains(&sql[0], r#""force_refresh" = "#);
    assert_contains(&sql[0], r#""matched_ref" = "#);
    assert_contains(&sql[0], "CAST(");
}

/// A library is queued by one statement bound by its id alone -- never a
/// parameter per title, which a library past 65,535 titles would
/// overflow -- reading its titles from both link tables, and a plain
/// refresh keeps the match.
#[tokio::test]
async fn refreshing_a_library_is_one_statement_bound_by_its_id_alone() {
    let queued = row([("queued", Value::BigInt(Some(7)))]);
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![queued.clone()], vec![queued]]),
    );
    let repo = SqlEnrichmentStateRepository::new(db.clone());
    let library = Uuid::from_u128(61);
    assert_eq!(
        repo.request_refresh_library(library, false).await.unwrap(),
        7
    );
    let _ = repo.request_refresh_library(library, true).await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 2, "one statement per refresh: {sql:?}");
    for statement in &sql {
        assert_eq!(
            bound_values(statement).len(),
            1,
            "only the library is bound: {statement:?}"
        );
        assert_bound(statement, &library.to_string());
        assert_contains(statement, "library_movies");
        assert_contains(statement, "library_shows");
        assert_contains(statement, "INSERT INTO metadata_enrichment");
        assert_contains(statement, "UPDATE metadata_enrichment");
    }
    assert!(!sql[0].sql.contains("matched_ref"), "{}", sql[0].sql);
    assert_contains(&sql[1], "matched_ref = NULL");
}

/// Locking upserts on the title's own unique column and writes only the
/// locks: a row's status and match are never reset by it.
#[tokio::test]
async fn locking_upserts_on_the_titles_column_and_updates_only_the_locks() {
    use beam_domain::models::enrichment::{EnrichmentTargetId, FieldLocks, MetadataField};

    let db = connection(empty_mock());
    let repo = SqlEnrichmentStateRepository::new(db.clone());
    let locks: FieldLocks = [MetadataField::Poster].into_iter().collect();
    let _ = repo
        .set_locked_fields(EnrichmentTargetId::Show(Uuid::from_u128(63)), &locks)
        .await;
    let _ = repo
        .set_locked_fields(EnrichmentTargetId::Movie(Uuid::from_u128(64)), &locks)
        .await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], r#"ON CONFLICT ("show_id") DO UPDATE SET"#);
    assert_contains(&sql[1], r#"ON CONFLICT ("movie_id") DO UPDATE SET"#);
    for insert in &sql[..2] {
        let set_list = insert
            .sql
            .split("DO UPDATE SET")
            .nth(1)
            .and_then(|rest| rest.split(" RETURNING").next())
            .unwrap_or_default();
        assert!(set_list.contains(r#""locked_fields""#), "{}", insert.sql);
        for untouched in ["\"status\"", "\"matched_ref\"", "\"attempts\""] {
            assert!(!set_list.contains(untouched), "{untouched}: {}", insert.sql);
        }
        assert_bound(insert, "poster");
    }
}

/// The list seeks past its cursor on `(updated_at, id)`, newest first,
/// under its filters, one row a page more than the caller shows.
#[tokio::test]
async fn the_list_seeks_newest_first_under_its_filters() {
    use beam_domain::models::catalog::TitleKind;
    use beam_domain::models::enrichment::{
        EnrichmentListFilter, EnrichmentListPosition, EnrichmentListQuery, EnrichmentStatus,
    };

    let db = connection(empty_mock());
    let repo = SqlEnrichmentStateRepository::new(db.clone());
    let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let _ = repo
        .list(&EnrichmentListQuery {
            filter: EnrichmentListFilter {
                status: Some(EnrichmentStatus::Failed),
                kind: Some(TitleKind::Show),
            },
            after: Some(EnrichmentListPosition {
                updated_at: at,
                id: Uuid::from_u128(65),
            }),
            limit: std::num::NonZeroU32::new(21).unwrap(),
        })
        .await;
    drop(repo);

    let sql = statements(db);
    let list = &sql[0];
    assert_filters(list, "metadata_enrichment", "status", "=");
    assert_filters(list, "metadata_enrichment", "show_id", "IS NOT NULL");
    assert_filters(list, "metadata_enrichment", "updated_at", "<");
    assert_filters(list, "metadata_enrichment", "id", "<");
    assert_contains(
        list,
        r#"ORDER BY "metadata_enrichment"."updated_at" DESC, "metadata_enrichment"."id" DESC"#,
    );
    assert_bound(list, "failed");
    assert_bound(list, "21");
    assert_bound(list, &Uuid::from_u128(65).to_string());
}
