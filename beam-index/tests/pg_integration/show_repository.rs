//! The shared `ShowRepository` contract, run against real SQL.
//!
//! Identical assertions to the in-memory instantiation in
//! `beam-domain/src/repositories/show.rs`, plus the one property only a real
//! Postgres can show: that `find_or_create_episode` is atomic under
//! concurrency.

use std::sync::Arc;

use beam_index::repositories::SqlShowRepository;
use beam_test_support::postgres;

struct PgFixture {
    repo: SqlShowRepository,
}

impl beam_domain::repositories::contract::fixture::ShowRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn beam_domain::repositories::ShowRepository {
        &self.repo
    }
}

async fn setup() -> PgFixture {
    PgFixture {
        repo: SqlShowRepository::new(postgres::connection().await),
    }
}

beam_domain::show_repository_contract!(setup);

/// The reason `find_or_create_episode` is `INSERT ... ON CONFLICT DO NOTHING`
/// rather than SELECT-then-INSERT: `(season_id, episode_number)` carries
/// `idx_episodes_unique`, so two files for one episode indexed at once would
/// both read "absent", both insert, and one would fail -- the scan rejecting a
/// perfectly good second source. The in-memory double holds a mutex across the
/// whole operation, so only a real Postgres can show this.
#[tokio::test]
async fn concurrent_find_or_create_episode_for_one_pair_all_succeed_with_one_row() {
    let db = postgres::connection().await;
    let repo = Arc::new(SqlShowRepository::new(db));
    let (season, _) = new_seasons(repo.as_ref()).await;

    let mut tasks = Vec::new();
    for i in 0..8u64 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.find_or_create_episode(episode(season, 1, &format!("rip {i}"), 40 + i))
                .await
        }));
    }

    let mut ids = Vec::new();
    for task in tasks {
        ids.push(
            task.await
                .expect("task did not panic")
                .expect("every concurrent call succeeds")
                .id,
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "every call returns the one episode: {ids:?}");
    assert_eq!(
        repo.find_episodes_by_season_id(season).await.unwrap().len(),
        1,
        "the unique (season_id, episode_number) index leaves exactly one row"
    );
}
