//! The shared `UserRepository` contract, run against real SQL.
//!
//! The same assertions the in-memory double is held to. That is what makes the
//! double legitimate scaffolding rather than a second, untested implementation.

use std::sync::Arc;

use beam_auth::utils::contract::fixture::UserRepositoryFixture;
use beam_auth::utils::repository::{SqlUserRepository, UserRepository};
use beam_domain::services::TestClock;
use beam_test_support::postgres::ScopedSchema;

/// Each instantiation gets its own migrated schema.
///
/// The contract asserts on `count()` and on a full-table `list_page` ordering.
/// Those are global statements about the whole `users` table, so they can only
/// hold if this test owns it -- on a shared database another test provisioning
/// a user mid-run makes them fail for a reason that has nothing to do with the
/// repository.
struct PgFixture {
    repo: SqlUserRepository,
    clock: Arc<TestClock>,
    // Kept alive for the fixture's lifetime; the schema is swept at the start
    // of the next run (see `drop_stale_scoped_schemas`).
    _schema: ScopedSchema,
}

#[async_trait::async_trait]
impl UserRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn UserRepository {
        &self.repo
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("users")
        .await
        .expect("a private migrated schema");
    // A fixed, non-epoch instant: `created_at` is `timestamptz`, and a clock
    // the contract advances must stay in a range Postgres accepts.
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ));
    PgFixture {
        repo: SqlUserRepository::with_clock(schema.db(), clock.clone()),
        clock,
        _schema: schema,
    }
}

beam_auth::user_repository_contract!(setup);
