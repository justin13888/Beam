//! The shared `ShowRepository` contract, run against real SQL.
//!
//! Identical assertions to the in-memory instantiation in
//! `beam-domain/src/repositories/show.rs`, plus the properties only a real
//! Postgres can show: that `find_or_create_episode` and
//! `find_or_create_by_identity` are atomic under concurrency.
//!
//! Each contract test runs in a migrated schema of its own: `delete_orphaned`
//! is global, and on the shared database it would delete other tests'
//! fileless shows while they run.

use std::sync::Arc;

use async_trait::async_trait;
// `Uuid` and `ShowRepository` come into scope with the contract macro below.
use beam_domain::repositories::FileRepository;
use beam_index::repositories::{SqlFileRepository, SqlShowRepository};
use beam_test_support::postgres::{self, ScopedSchema};
use beam_test_support::seed;

struct PgFixture {
    schema: ScopedSchema,
    repo: SqlShowRepository,
    files: SqlFileRepository,
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::ShowRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn ShowRepository {
        &self.repo
    }

    fn files(&self) -> &dyn FileRepository {
        &self.files
    }

    async fn new_library(&self) -> Uuid {
        seed::library(self.schema.db().as_ref())
            .await
            .expect("seed a library")
    }

    async fn new_unkeyed_show(
        &self,
        title: &str,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        use sea_orm::{ActiveModelTrait, Set};

        let id = Uuid::new_v4();
        let now: chrono::DateTime<chrono::FixedOffset> = created_at.into();
        beam_entity::show::ActiveModel {
            id: Set(id),
            title: Set(title.to_string()),
            identity_key: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.schema.db().as_ref())
        .await
        .expect("insert a show with no identity key");
        id
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("show_contract")
        .await
        .expect("create a migrated schema");
    PgFixture {
        repo: SqlShowRepository::new(schema.db()),
        files: SqlFileRepository::new(schema.db()),
        schema,
    }
}

beam_domain::show_repository_contract!(setup);

/// The reason `find_or_create_episode` is `INSERT ... ON CONFLICT DO NOTHING`
/// rather than SELECT-then-INSERT: `(season_id, episode_number)` carries
/// `idx_episodes_unique`, so two files for one episode indexed at once would
/// both read "absent", both insert, and one would fail -- the scan rejecting a
/// perfectly good second source. The in-memory double holds a mutex across the
/// whole operation, so only a real Postgres can show this.
#[tokio::test]
async fn concurrent_find_or_create_episode_for_one_pair_all_succeed_with_one_row() {
    let db = postgres::connection().await;
    let repo = Arc::new(SqlShowRepository::new(db));
    let (season, _) = new_seasons(repo.as_ref()).await;

    let mut tasks = Vec::new();
    for i in 0..8u64 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.find_or_create_episode(episode(season, 1, &format!("rip {i}"), 40 + i))
                .await
        }));
    }

    let mut ids = Vec::new();
    for task in tasks {
        ids.push(
            task.await
                .expect("task did not panic")
                .expect("every concurrent call succeeds")
                .id,
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "every call returns the one episode: {ids:?}");
    assert_eq!(
        repo.find_episodes_by_season_id(season).await.unwrap().len(),
        1,
        "the unique (season_id, episode_number) index leaves exactly one row"
    );
}

/// Issue #183 replaced `find_by_title`-then-`create` with one `ON CONFLICT
/// (identity_key)` statement. Under the old pair two episodes of a new show,
/// indexed at once by a scan and the watcher, both read "absent" and made two
/// shows. Only a real Postgres can show the race is gone.
#[tokio::test]
async fn concurrent_find_or_create_by_identity_for_one_key_all_succeed_with_one_row() {
    use beam_domain::models::CreateShow;

    let db = postgres::connection().await;
    let repo = Arc::new(SqlShowRepository::new(db));
    let title = format!("Race Show {}", Uuid::new_v4());

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        let create = CreateShow::new(title.clone(), Some(2024));
        tasks.push(tokio::spawn(async move {
            repo.find_or_create_by_identity(create).await
        }));
    }

    let mut ids = Vec::new();
    for task in tasks {
        ids.push(
            task.await
                .expect("task did not panic")
                .expect("every concurrent call succeeds")
                .id,
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "every call returns the one show: {ids:?}");
}
