//! The shared `AdminLogRepository` contract (issue #349), run against real
//! SQL: the `admin_log_level` and `admin_log_category` Postgres enums every
//! entry is stored through, `created_at` ordering, and offset pages.
//!
//! Listing and counting are global, so each test owns a migrated schema.

use std::sync::Arc;

use beam_domain::repositories::AdminLogRepository;
use beam_domain::services::TestClock;
use beam_index::repositories::SqlAdminLogRepository;
use beam_test_support::postgres::ScopedSchema;

struct PgFixture {
    // Held so the schema outlives the repository that uses it.
    _schema: ScopedSchema,
    repo: SqlAdminLogRepository,
    clock: Arc<TestClock>,
}

impl beam_domain::repositories::contract::fixture::AdminLogFixture for PgFixture {
    fn repo(&self) -> &dyn AdminLogRepository {
        &self.repo
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("admin_log_contract")
        .await
        .expect("create a migrated schema");
    let clock = Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_900_000_000, 0).unwrap(),
    ));
    PgFixture {
        repo: SqlAdminLogRepository::with_clock(schema.db(), clock.clone()),
        clock,
        _schema: schema,
    }
}

beam_domain::admin_log_repository_contract!(setup);
