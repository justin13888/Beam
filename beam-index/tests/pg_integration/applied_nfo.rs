//! The shared `AppliedNfoRepository` contract, run against real SQL, plus
//! what only a real Postgres can show: that recording by path is one atomic
//! statement, and that a record goes with its library.

// `PathBuf`, `Uuid` and the applied-NFO models come into scope with the
// contract macro below.
use std::sync::Arc;

use async_trait::async_trait;
use beam_domain::repositories::AppliedNfoRepository;
use beam_index::repositories::SqlAppliedNfoRepository;
use beam_test_support::{postgres, seed};
use sea_orm::DatabaseConnection;

struct PgFixture {
    db: Arc<DatabaseConnection>,
    repo: SqlAppliedNfoRepository,
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::AppliedNfoFixture for PgFixture {
    fn repo(&self) -> &dyn AppliedNfoRepository {
        &self.repo
    }

    async fn new_library(&self) -> Uuid {
        seed::library(self.db.as_ref())
            .await
            .expect("seed a library")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    PgFixture {
        repo: SqlAppliedNfoRepository::new(db.clone()),
        db,
    }
}

beam_domain::applied_nfo_repository_contract!(setup);

fn record(library_id: Uuid, path: &PathBuf) -> RecordAppliedNfo {
    RecordAppliedNfo {
        library_id,
        path: path.clone(),
        size_bytes: 10,
        content_hash: "c0ffee".to_string(),
        change_stamp: None,
    }
}

/// A scan and a watcher event can apply one NFO at once. Recording is one
/// `ON CONFLICT (path)` statement, so both succeed and one row is left.
#[tokio::test]
async fn concurrent_records_of_one_path_all_succeed_and_leave_one_row() {
    let fixture = setup().await;
    let library = seed::library(fixture.db.as_ref()).await.unwrap();
    let path = PathBuf::from(format!("/videos/{}/movie.nfo", Uuid::new_v4()));
    let repo = Arc::new(SqlAppliedNfoRepository::new(fixture.db.clone()));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        let record = record(library, &path);
        tasks.push(tokio::spawn(
            async move { repo.record_by_path(record).await },
        ));
    }
    for task in tasks {
        task.await
            .expect("task did not panic")
            .expect("every concurrent record succeeds");
    }
    assert_eq!(repo.find_all_by_library(library).await.unwrap().len(), 1);
}

/// Deleting a library takes its NFO records with it (`ON DELETE CASCADE`).
#[tokio::test]
async fn deleting_a_library_takes_its_nfo_records_with_it() {
    use beam_domain::repositories::LibraryRepository;
    use beam_index::repositories::library::SqlLibraryRepository;

    let fixture = setup().await;
    let library = seed::library(fixture.db.as_ref()).await.unwrap();
    let path = PathBuf::from(format!("/videos/{}/movie.nfo", Uuid::new_v4()));
    fixture
        .repo
        .record_by_path(record(library, &path))
        .await
        .unwrap();

    SqlLibraryRepository::new(fixture.db.clone())
        .delete(library)
        .await
        .expect("delete the library");

    assert_eq!(fixture.repo.find_by_path(&path).await.unwrap(), None);
}
