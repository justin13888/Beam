//! The shared `WatchStateRepository` contract, run against real SQL.
//!
//! Identical assertions to the in-memory instantiation in
//! `beam-domain/src/repositories/watch_state.rs`. That is the entire point:
//! the fake is only legitimate scaffolding while the same suite constrains
//! the implementation it stands in for, so any divergence between them fails
//! here instead of drifting silently.

use std::sync::Arc;

// `Uuid` is brought into scope by the contract macro below.
use uuid::Uuid as FixtureUuid;

use beam_domain::models::watch_state::WatchTarget as FixtureTarget;
use beam_domain::repositories::WatchStateRepository;
use beam_domain::repositories::contract::fixture::WatchStateFixture;
use beam_domain::services::TestClock;
use beam_index::repositories::watch_state::SqlWatchStateRepository;
use beam_test_support::{postgres, seed};

struct PgFixture {
    repo: SqlWatchStateRepository,
    clock: Arc<TestClock>,
    db: Arc<sea_orm::DatabaseConnection>,
}

#[async_trait::async_trait]
impl WatchStateFixture for PgFixture {
    fn repo(&self) -> &dyn WatchStateRepository {
        &self.repo
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }

    async fn new_user(&self) -> FixtureUuid {
        seed::user(&self.db).await.expect("seed a user row")
    }

    async fn new_movie(&self) -> FixtureTarget {
        FixtureTarget::Movie {
            movie_id: seed::movie(&self.db).await.expect("seed a movie row"),
        }
    }

    async fn new_show(&self) -> FixtureUuid {
        seed::show(&self.db).await.expect("seed a show row")
    }

    async fn new_episode(&self, show_id: FixtureUuid) -> FixtureTarget {
        FixtureTarget::Episode {
            episode_id: seed::episode_of(&self.db, show_id)
                .await
                .expect("seed an episode row"),
            show_id,
        }
    }

    async fn new_file(&self) -> FixtureUuid {
        seed::file(&self.db).await.expect("seed a file row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    // Start from a fixed, non-epoch instant: `last_played_at` is
    // `timestamptz`, and a clock the contract advances must stay inside a
    // range Postgres accepts.
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        repo: SqlWatchStateRepository::with_clock(db.clone(), clock.clone()),
        clock,
        db,
    }
}

beam_domain::watch_state_repository_contract!(setup);

/// Why a report is one `ON CONFLICT` statement rather than a
/// SELECT-then-UPDATE-or-INSERT: the (user, movie) pair carries a unique
/// index, so concurrent reports race the read-modify-write -- both read
/// "absent", both insert, and one fails. Only a real Postgres can show this;
/// the in-memory double holds a mutex across the whole operation.
#[tokio::test]
async fn concurrent_reports_for_one_title_all_succeed_and_leave_one_row() {
    let fixture = setup().await;
    let user = fixture.new_user().await;
    let movie = fixture.new_movie().await;
    let file = fixture.new_file().await;

    let repo = Arc::new(SqlWatchStateRepository::with_clock(
        fixture.db.clone(),
        fixture.clock.clone(),
    ));
    let mut tasks = Vec::new();
    for i in 0..8u32 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.record_progress(beam_domain::models::watch_state::RecordProgress {
                user_id: user,
                target: movie,
                file_id: file,
                position_secs: f64::from(i),
                duration_secs: Some(100.0),
            })
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
        "the unique (user_id, movie_id) index leaves exactly one row"
    );
}
