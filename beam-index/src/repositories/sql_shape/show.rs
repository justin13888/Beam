use super::*;
use beam_domain::models::CreateEpisode;
use beam_domain::repositories::ShowRepository;

use crate::repositories::SqlShowRepository;

#[tokio::test]
async fn outlines_of_a_page_of_shows_are_one_statement_and_none_for_no_show() {
    let db = connection(empty_mock());
    let repo = SqlShowRepository::new(db.clone());
    let _ = repo.episode_outlines(&[]).await;
    let _ = repo
        .episode_outlines(&[Uuid::from_u128(1), Uuid::from_u128(2)])
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "an empty slice reads nothing");
    assert_contains(&sql[0], "WHERE se.show_id IN ($1, $2)");
    assert_contains(
        &sql[0],
        "ORDER BY se.show_id, se.season_number, e.episode_number",
    );
    assert_bound(&sql[0], &Uuid::from_u128(2).to_string());
}

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
            air_date: None,
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
