//! The shared `MediaStreamRepository` contract (issue #349), run against real
//! SQL: every metadata field through its column, the `stream_type` enum, and
//! the index order a file's streams read back in.

use std::sync::Arc;

use async_trait::async_trait;
use beam_domain::repositories::MediaStreamRepository;
use beam_index::repositories::SqlMediaStreamRepository;
use beam_test_support::{postgres, seed};
use sea_orm::DatabaseConnection;

struct PgFixture {
    db: Arc<DatabaseConnection>,
    repo: SqlMediaStreamRepository,
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::MediaStreamFixture for PgFixture {
    fn repo(&self) -> &dyn MediaStreamRepository {
        &self.repo
    }

    async fn new_file(&self) -> Uuid {
        seed::file(self.db.as_ref()).await.expect("seed a file")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    PgFixture {
        repo: SqlMediaStreamRepository::new(db.clone()),
        db,
    }
}

beam_domain::media_stream_repository_contract!(setup);
