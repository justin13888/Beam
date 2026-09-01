//! The shared `ShowRepository` contract, run against real SQL.
//!
//! This instantiation is the one that matters. `find_or_create_episode` exists
//! because `episodes` carries `idx_episodes_unique` over
//! `(season_id, episode_number)`: the previous unconditional insert raised a
//! uniqueness violation the moment a season held a second file for one
//! episode, and that aborted the entire library scan (#142). An in-memory
//! `HashMap` has no unique index and cannot reproduce that failure, so the
//! same suite is bound here, where the constraint actually exists.

use std::sync::Arc;

use beam_domain::repositories::ShowRepository;
use beam_domain::repositories::contract::fixture::ShowFixture;
use beam_index::repositories::show::SqlShowRepository;
use beam_test_support::postgres;

struct PgFixture {
    repo: SqlShowRepository,
}

impl ShowFixture for PgFixture {
    fn repo(&self) -> &dyn ShowRepository {
        &self.repo
    }
}

async fn setup() -> PgFixture {
    let db: Arc<sea_orm::DatabaseConnection> = postgres::connection().await;
    PgFixture {
        repo: SqlShowRepository::new(db),
    }
}

beam_domain::show_repository_contract!(setup);
