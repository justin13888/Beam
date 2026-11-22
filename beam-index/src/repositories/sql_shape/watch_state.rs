use super::*;
use beam_domain::models::watch_state::{
    ContinueCandidate, HistoryPosition, RecordProgress, TitleRef, WatchTarget,
};
use beam_domain::repositories::WatchStateRepository;

use crate::repositories::SqlWatchStateRepository;

fn movie(n: u128) -> WatchTarget {
    WatchTarget::Movie {
        movie_id: Uuid::from_u128(n),
    }
}

fn episode(n: u128) -> WatchTarget {
    WatchTarget::Episode {
        episode_id: Uuid::from_u128(n),
        show_id: Uuid::from_u128(999),
    }
}

/// The statements that write, leaving out the transaction's own.
fn writes(sql: &[Statement]) -> Vec<&Statement> {
    sql.iter()
        .filter(|s| {
            let head = s.sql.trim_start();
            head.starts_with("INSERT") || head.starts_with("UPDATE") || head.starts_with("DELETE")
        })
        .collect()
}

#[tokio::test]
async fn a_report_is_one_upsert_on_the_titles_unique_index() {
    for (target, column) in [(movie(1), "movie_id"), (episode(2), "episode_id")] {
        let db = connection(empty_mock());
        let repo = SqlWatchStateRepository::new(db.clone());
        let _ = repo
            .record_progress(RecordProgress {
                user_id: Uuid::from_u128(7),
                target,
                file_id: Uuid::from_u128(8),
                position_secs: 12.0,
                duration_secs: Some(100.0),
                finishes_title: true,
            })
            .await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 1, "a report must be a single statement");
        assert_contains(
            &sql[0],
            &format!("ON CONFLICT (user_id, {column}) DO UPDATE"),
        );
        // Played is sticky and a play is counted only off the end.
        assert_contains(
            &sql[0],
            "completed = watch_state.completed OR excluded.completed",
        );
        assert_contains(&sql[0], "THEN watch_state.play_count + 1");
        assert!(
            !sql[0].sql.contains("id = excluded.id"),
            "the primary key must survive the conflict:\n{}",
            sql[0].sql
        );
        // By position: the real clock is bound too, and its seconds can
        // read "12.0" whatever the position was.
        let values = &sql[0]
            .values
            .as_ref()
            .expect("the upsert binds its values")
            .0;
        assert_eq!(
            values[5],
            Value::from(Uuid::from_u128(8)),
            "$6 (last_file_id)"
        );
        assert_eq!(values[6], Value::Double(Some(12.0)), "$7 (position_secs)");
        assert_eq!(values[8], Value::Bool(Some(false)), "$9 (completed)");
    }
}

#[tokio::test]
async fn a_report_past_the_end_writes_the_start_and_played() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    let _ = repo
        .record_progress(RecordProgress {
            user_id: Uuid::from_u128(7),
            target: movie(1),
            file_id: Uuid::from_u128(8),
            position_secs: 97.0,
            duration_secs: Some(100.0),
            finishes_title: true,
        })
        .await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], "position_secs, duration_secs, completed");
    assert_contains(&sql[0], "VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,");
    // By position, not by substring: the row id and the clock are bound
    // too, and either can contain any digit run.
    let values = &sql[0]
        .values
        .as_ref()
        .expect("the upsert binds its values")
        .0;
    assert_eq!(
        values[6],
        Value::Double(Some(0.0)),
        "$7 (position_secs): the end is stored as the start"
    );
    assert_eq!(values[7], Value::Double(Some(100.0)), "$8 (duration_secs)");
    assert_eq!(values[8], Value::Bool(Some(true)), "$9 (completed)");
}

#[tokio::test]
async fn marking_nothing_issues_no_statement() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    repo.mark_played(Uuid::nil(), &[]).await.unwrap();
    repo.mark_unplayed(Uuid::nil(), &[]).await.unwrap();
    drop(repo);

    assert!(statements(db).is_empty());
}

#[tokio::test]
async fn marking_writes_one_statement_per_index_without_repeats() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    repo.mark_played(
        Uuid::from_u128(7),
        &[episode(2), movie(1), episode(3), episode(2)],
    )
    .await
    .unwrap();
    drop(repo);

    let sql = statements(db);
    let writes = writes(&sql);
    assert_eq!(writes.len(), 2, "{sql:?}");
    assert_contains(writes[0], "ON CONFLICT (user_id, movie_id) DO UPDATE");
    assert_contains(writes[1], "ON CONFLICT (user_id, episode_id) DO UPDATE");
    assert_eq!(
        writes[1].sql.matches(", true, 1, $2)").count(),
        2,
        "a repeated episode would touch its row twice, which Postgres refuses:\n{}",
        writes[1].sql
    );
    // A title already at its end keeps its count and its time.
    assert_contains(writes[0], "THEN watch_state.play_count");
    assert_contains(writes[0], "THEN watch_state.last_played_at");
}

#[tokio::test]
async fn dismissing_binds_the_title_column() {
    for (title, column) in [
        (TitleRef::Movie(Uuid::from_u128(1)), "movie_id"),
        (TitleRef::Show(Uuid::from_u128(2)), "show_id"),
    ] {
        let db = connection(empty_mock());
        let repo = SqlWatchStateRepository::new(db.clone());
        repo.dismiss(Uuid::from_u128(7), title).await.unwrap();
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 1);
        assert_contains(&sql[0], "SET dismissed_at = $3");
        assert_contains(&sql[0], &format!("AND {column} = $2"));
        assert_bound(&sql[0], &Uuid::from_u128(7).to_string());
    }
}

#[tokio::test]
async fn clearing_deletes_only_an_unplayed_row_then_rewinds() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    repo.clear_progress(Uuid::from_u128(7), episode(2))
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    let writes = writes(&sql);
    assert_eq!(writes.len(), 2);
    assert_contains(writes[0], "DELETE FROM watch_state");
    assert_contains(writes[0], "episode_id = $2 AND NOT completed");
    assert_contains(writes[1], "SET position_secs = 0");
}

#[tokio::test]
async fn candidates_group_per_title_and_seek_past_the_last_read() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    let after = ContinueCandidate {
        title: TitleRef::Show(Uuid::from_u128(5)),
        last_played_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    };
    let _ = repo
        .find_continue_candidates(Uuid::from_u128(7), None, 20)
        .await;
    let _ = repo
        .find_continue_candidates(Uuid::from_u128(7), Some(after), 20)
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 2);
    for statement in &sql {
        assert_contains(statement, "GROUP BY movie_id, show_id");
        assert_contains(
            statement,
            "HAVING max(last_played_at) > coalesce(max(dismissed_at), '-infinity')",
        );
        assert_contains(statement, "bool_or(position_secs > 0)");
        assert_contains(
            statement,
            "ORDER BY max(last_played_at) DESC, coalesce(movie_id, show_id) DESC",
        );
        assert_contains(statement, "LIMIT $2");
        assert!(!statement.sql.contains("OFFSET"), "{}", statement.sql);
        assert_bound(statement, "20");
    }
    assert!(!sql[0].sql.contains("< ($3, $4)"), "{}", sql[0].sql);
    assert_contains(
        &sql[1],
        "(max(last_played_at), coalesce(movie_id, show_id)) < ($3, $4)",
    );
    assert_bound(&sql[1], &Uuid::from_u128(5).to_string());
    assert_bound(&sql[1], "2023-11-14");
}

#[tokio::test]
async fn a_page_of_shows_is_read_in_one_statement_and_none_is_read_for_no_show() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    let _ = repo.find_for_shows(Uuid::from_u128(7), &[]).await;
    let _ = repo
        .find_for_shows(
            Uuid::from_u128(7),
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
        )
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "an empty slice reads nothing");
    assert_contains(&sql[0], "WHERE user_id = $1 AND show_id IN ($2, $3)");
    assert_bound(&sql[0], &Uuid::from_u128(2).to_string());
}

#[tokio::test]
async fn a_history_page_seeks_past_its_position_newest_first() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    let after = HistoryPosition {
        last_played_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        id: Uuid::from_u128(5),
    };
    let _ = repo
        .find_history_page(Uuid::from_u128(7), Some(after), 25)
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1);
    assert_contains(&sql[0], "(last_played_at, id) < ($2, $3)");
    assert_contains(&sql[0], "ORDER BY last_played_at DESC, id DESC LIMIT $4");
    assert_bound(&sql[0], &Uuid::from_u128(5).to_string());
    assert_bound(&sql[0], "25");
}

#[tokio::test]
async fn last_played_at_asks_nothing_of_no_files() {
    let db = connection(empty_mock());
    let repo = SqlWatchStateRepository::new(db.clone());
    assert!(repo.last_played_at(Vec::new()).await.unwrap().is_empty());
    drop(repo);

    assert!(statements(db).is_empty());
}
