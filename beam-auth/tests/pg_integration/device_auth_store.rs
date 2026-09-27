//! The shared `DeviceAuthStore` contract, run against real SQL.
//!
//! Claiming a poll is a conditional `UPDATE`, and ending a flow is a
//! `DELETE ... RETURNING`; only a real Postgres can show that concurrent
//! callers get exactly one winner from each.

use std::sync::Arc;

use beam_auth::utils::contract::fixture::DeviceAuthStoreFixture;
use beam_auth::utils::device_auth_store::{DeviceAuthStore, SqlDeviceAuthStore};
use beam_domain::services::TestClock;
use beam_test_support::postgres::ScopedSchema;

/// Each instantiation gets its own migrated schema.
///
/// Starting a flow sweeps every expired flow in the table, and the contract
/// moves its clock far past a flow's lifetime to prove it. On a shared
/// database that sweep deletes other tests' flows from under them, so each
/// test owns its `device_auths` table.
struct PgFixture {
    store: SqlDeviceAuthStore,
    clock: Arc<TestClock>,
    // Kept alive for the fixture's lifetime; the schema is swept at the start
    // of the next run (see `drop_stale_scoped_schemas`).
    _schema: ScopedSchema,
}

#[async_trait::async_trait]
impl DeviceAuthStoreFixture for PgFixture {
    fn store(&self) -> &dyn DeviceAuthStore {
        &self.store
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("device_auths")
        .await
        .expect("a private migrated schema");
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        store: SqlDeviceAuthStore::with_clock(schema.db(), clock.clone()),
        clock,
        _schema: schema,
    }
}

// The contract brings `Claim` and `NewDeviceAuth` into scope for the tests
// below as well.
beam_auth::device_auth_store_contract!(setup);

fn flow() -> NewDeviceAuth {
    NewDeviceAuth {
        handle_hash: uuid::Uuid::new_v4().simple().to_string(),
        device_code: "device-code".to_string(),
        user_code: "BCDF-GHJK".to_string(),
        verification_uri: "https://idp.test/device".to_string(),
        verification_uri_complete: None,
        interval_secs: 5,
        expires_in_secs: 600,
    }
}

/// Two polls of one flow arriving together must reach the IdP once: the
/// second has to see the first's claim, not the row both of them read.
#[tokio::test]
async fn concurrent_polls_of_one_flow_claim_it_exactly_once() {
    let fixture = setup().await;
    let flow = flow();
    fixture.store.create(&flow).await.unwrap();

    let store = Arc::new(fixture.store);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let handle_hash = flow.handle_hash.clone();
        tasks.push(tokio::spawn(
            async move { store.claim_poll(&handle_hash).await },
        ));
    }

    let mut claimed = 0;
    for task in tasks {
        match task.await.unwrap().unwrap() {
            Claim::Claimed(_) => claimed += 1,
            Claim::TooEarly { .. } => {}
            other => panic!("a live flow polled concurrently answered {other:?}"),
        }
    }
    assert_eq!(claimed, 1, "exactly one concurrent poll may reach the IdP");
}

/// One approval, many concurrent finishers: only one may mint a session.
#[tokio::test]
async fn concurrent_consumers_of_one_flow_produce_exactly_one_winner() {
    let fixture = setup().await;
    let flow = flow();
    fixture.store.create(&flow).await.unwrap();

    let store = Arc::new(fixture.store);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let handle_hash = flow.handle_hash.clone();
        tasks.push(tokio::spawn(
            async move { store.consume(&handle_hash).await },
        ));
    }

    let mut winners = 0;
    for task in tasks {
        if task.await.unwrap().unwrap().is_some() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "exactly one concurrent finisher may end a flow");
}
