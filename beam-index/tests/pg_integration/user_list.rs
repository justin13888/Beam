//! The shared `UserListRepository` contract, run against real SQL: the
//! per-kind unique indexes `ON CONFLICT DO NOTHING` relies on, the foreign
//! keys, and the carry a title merge makes (issue #262).

use std::sync::Arc;

use uuid::Uuid as FixtureUuid;

use beam_domain::repositories::contract::fixture::UserListFixture;
use beam_domain::repositories::{MovieRepository, ShowRepository, UserListRepository};
use beam_domain::services::TestClock;
use beam_index::repositories::{SqlMovieRepository, SqlShowRepository, SqlUserListRepository};
use beam_test_support::{postgres, seed};

struct PgFixture {
    repo: SqlUserListRepository,
    clock: Arc<TestClock>,
    db: Arc<sea_orm::DatabaseConnection>,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
}

#[async_trait::async_trait]
impl UserListFixture for PgFixture {
    fn repo(&self) -> &dyn UserListRepository {
        &self.repo
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }

    async fn new_library(&self) -> FixtureUuid {
        seed::library(&self.db).await.expect("seed a library")
    }

    async fn new_user(&self) -> FixtureUuid {
        seed::user(&self.db).await.expect("seed a user row")
    }

    async fn new_movie(&self) -> FixtureUuid {
        seed::movie(&self.db).await.expect("seed a movie row")
    }

    async fn new_show(&self) -> FixtureUuid {
        seed::show(&self.db).await.expect("seed a show row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    // A fixed, non-epoch start, as for watch state: `added_at` is
    // `timestamptz`.
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        repo: SqlUserListRepository::with_clock(db.clone(), clock.clone()),
        clock,
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db.clone()),
        db,
    }
}

beam_domain::user_list_repository_contract!(setup);

/// Why an addition is one `INSERT ... ON CONFLICT DO NOTHING`: the
/// (user, list, title) triple carries a unique index, so concurrent additions
/// race a read-then-insert -- both read "absent", both insert, and one fails.
/// Only a real Postgres can show this.
#[tokio::test]
async fn concurrent_additions_of_one_title_all_succeed_and_leave_one_entry() {
    use beam_domain::models::user_list::UserList;
    use beam_domain::models::watch_state::TitleRef;

    let fixture = setup().await;
    let user = fixture.new_user().await;
    let movie = TitleRef::Movie(fixture.new_movie().await);
    let repo = Arc::new(SqlUserListRepository::with_clock(
        fixture.db.clone(),
        fixture.clock.clone(),
    ));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.add(user, UserList::Watchlist, movie).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(
            task.await
                .expect("task did not panic")
                .expect("every concurrent addition succeeds")
                .id,
        );
    }

    ids.dedup();
    assert_eq!(ids.len(), 1, "every addition answers the one entry");
    assert_eq!(
        repo.count(
            user,
            UserList::Watchlist,
            &beam_domain::models::access::AccessPolicy::unrestricted()
        )
        .await
        .unwrap(),
        1
    );
}
