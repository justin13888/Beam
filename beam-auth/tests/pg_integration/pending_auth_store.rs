//! The shared `PendingAuthStore` contract, run against real SQL.
//!
//! Single-use consumption is a `DELETE ... RETURNING` in the SQL
//! implementation, deliberately not a SELECT-then-DELETE. Only a real Postgres
//! can show that it holds.

use std::sync::Arc;

use beam_auth::utils::contract::fixture::PendingAuthStoreFixture;
use beam_auth::utils::pending_auth_store::{PendingAuthStore, SqlPendingAuthStore};
use beam_domain::services::TestClock;
use beam_test_support::postgres::ScopedSchema;

/// Each instantiation gets its own migrated schema.
///
/// Starting a login sweeps every expired pending authorization in the table,
/// and the contract moves its clock past the TTL to prove it. On a shared
/// database that sweep deletes other tests' rows from under them, so each
/// test owns its `pending_auths` table.
struct PgFixture {
    store: SqlPendingAuthStore,
    clock: Arc<TestClock>,
    // Kept alive for the fixture's lifetime; the schema is swept at the start
    // of the next run (see `drop_stale_scoped_schemas`).
    _schema: ScopedSchema,
}

#[async_trait::async_trait]
impl PendingAuthStoreFixture for PgFixture {
    fn store(&self) -> &dyn PendingAuthStore {
        &self.store
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("pending_auths")
        .await
        .expect("a private migrated schema");
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        store: SqlPendingAuthStore::with_clock(schema.db(), clock.clone()),
        clock,
        _schema: schema,
    }
}

beam_auth::pending_auth_store_contract!(setup);

/// The reason `consume` is a single `DELETE ... RETURNING` rather than a
/// SELECT followed by a DELETE: under Postgres's default READ COMMITTED
/// isolation, two concurrent consumers of the same `state` can both observe
/// the row before either delete commits, and both would complete a login.
#[tokio::test]
async fn concurrent_consumers_of_one_state_produce_exactly_one_winner() {
    let fixture = setup().await;
    let state = uuid::Uuid::new_v4().to_string();
    fixture
        .store
        .create(
            &beam_auth::utils::pending_auth_store::test_utils::pending_auth(&state),
            600,
        )
        .await
        .unwrap();

    let store = Arc::new(fixture.store);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let state = state.clone();
        tasks.push(tokio::spawn(async move { store.consume(&state).await }));
    }

    let mut winners = 0;
    for task in tasks {
        if task.await.unwrap().unwrap().is_some() {
            winners += 1;
        }
    }

    assert_eq!(
        winners, 1,
        "exactly one concurrent consumer may exchange a given state"
    );
}
