//! The shared `PlaybackProgressRepository` contract, run against real SQL.
//!
//! Identical assertions to the in-memory instantiation in
//! `beam-domain/src/repositories/playback_progress.rs`. That is the entire
//! point: the fake is only legitimate scaffolding while the same suite
//! constrains the implementation it stands in for, so any divergence between
//! them fails here instead of drifting silently.

use std::sync::Arc;

// `Uuid` is brought into scope by the contract macro below.
use uuid::Uuid as FixtureUuid;

use beam_domain::repositories::PlaybackProgressRepository;
use beam_domain::repositories::contract::fixture::PlaybackProgressFixture;
use beam_domain::services::TestClock;
use beam_index::repositories::playback_progress::SqlPlaybackProgressRepository;
use beam_test_support::{postgres, seed};

struct PgFixture {
    repo: SqlPlaybackProgressRepository,
    clock: Arc<TestClock>,
    db: Arc<sea_orm::DatabaseConnection>,
}

#[async_trait::async_trait]
impl PlaybackProgressFixture for PgFixture {
    fn repo(&self) -> &dyn PlaybackProgressRepository {
        &self.repo
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }

    async fn new_user(&self) -> FixtureUuid {
        seed::user(&self.db).await.expect("seed a user row")
    }

    async fn new_file(&self) -> FixtureUuid {
        seed::file(&self.db).await.expect("seed a file row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    // Start from a fixed, non-epoch instant: `updated_at` is `timestamptz`, and
    // a clock the contract advances must stay inside a range Postgres accepts.
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        repo: SqlPlaybackProgressRepository::with_clock(db.clone(), clock.clone()),
        clock,
        db,
    }
}

beam_domain::playback_progress_repository_contract!(setup);

/// The reason `upsert` is a single `ON CONFLICT` statement rather than a
/// SELECT-then-UPDATE-or-INSERT: `(user_id, file_id)` carries a unique index,
/// so concurrent reports for the same pair race the read-modify-write -- both
/// read "absent", both insert, and one fails. Only a real Postgres can show
/// this; the in-memory double holds a mutex across the whole operation.
#[tokio::test]
async fn concurrent_upserts_for_one_pair_all_succeed_and_leave_one_row() {
    let fixture = setup().await;
    let user = fixture.new_user().await;
    let file = fixture.new_file().await;

    let repo = Arc::new(SqlPlaybackProgressRepository::with_clock(
        fixture.db.clone(),
        fixture.clock.clone(),
    ));
    let mut tasks = Vec::new();
    for i in 0..8u32 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.upsert(
                beam_domain::models::playback_progress::UpsertPlaybackProgress {
                    user_id: user,
                    file_id: file,
                    position_secs: f64::from(i),
                    duration_secs: Some(100.0),
                },
            )
            .await
        }));
    }

    for task in tasks {
        task.await
            .expect("task did not panic")
            .expect("every concurrent report succeeds");
    }

    assert_eq!(
        repo.count_by_user(user).await.unwrap(),
        1,
        "the unique (user_id, file_id) index leaves exactly one row"
    );
}
