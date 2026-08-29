//! The shared `SessionStore` contract, run against real SQL.

use std::sync::Arc;

use beam_auth::utils::contract::fixture::SessionStoreFixture;
use beam_auth::utils::session_store::{PgSessionStore, SessionStore};
use beam_domain::services::TestClock;
use beam_test_support::{postgres, seed};

struct PgFixture {
    store: PgSessionStore,
    clock: Arc<TestClock>,
    db: Arc<sea_orm::DatabaseConnection>,
}

#[async_trait::async_trait]
impl SessionStoreFixture for PgFixture {
    fn store(&self) -> &dyn SessionStore {
        &self.store
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }

    async fn new_user(&self) -> uuid::Uuid {
        // A real Postgres enforces `sessions.user_id -> users.id`, so a bare
        // v4 UUID is not a usable owner here.
        seed::user(&self.db).await.expect("seed a user row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        store: PgSessionStore::with_clock(db.clone(), clock.clone()),
        clock,
        db,
    }
}

beam_auth::session_store_contract!(setup);
