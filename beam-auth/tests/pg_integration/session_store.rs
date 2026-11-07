//! The shared `SessionStore` contract, run against real SQL.

use std::sync::Arc;

use beam_auth::utils::contract::fixture::SessionStoreFixture;
use beam_auth::utils::session_store::{PgSessionStore, SessionStore};
use beam_domain::services::TestClock;
use beam_test_support::{postgres::ScopedSchema, seed};

/// Each instantiation gets its own migrated schema.
///
/// Creating a session sweeps every expired session in the table, and the
/// contract moves its clock past session deadlines to prove it. On a shared
/// database that sweep deletes other tests' sessions from under them, so each
/// test owns its `sessions` table.
struct PgFixture {
    store: PgSessionStore,
    clock: Arc<TestClock>,
    db: Arc<sea_orm::DatabaseConnection>,
    // Kept alive for the fixture's lifetime; the schema is swept at the start
    // of the next run (see `drop_stale_scoped_schemas`).
    _schema: ScopedSchema,
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
    let schema = ScopedSchema::create_migrated("sessions")
        .await
        .expect("a private migrated schema");
    let db = schema.db();
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        store: PgSessionStore::with_clock(db.clone(), clock.clone()),
        clock,
        db,
        _schema: schema,
    }
}

beam_auth::session_store_contract!(setup);
