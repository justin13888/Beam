//! The shared `EnrichmentStateRepository` contract (issue #185), run against
//! real SQL: locks held to the known field names by the column's `CHECK`, the
//! lock upsert on each title's unique column, the one-statement refreshes, and
//! the admin list's keyset pages -- plus what only a real Postgres can show:
//! that concurrent locks of one title leave one row, and that the column
//! refuses a name no field has.
//!
//! Listing, counting and refreshing everything are global, so each test owns
//! a migrated schema.

use std::sync::Arc;

use beam_domain::repositories::{EnrichmentStateRepository, MovieRepository, ShowRepository};
use beam_index::repositories::{
    SqlEnrichmentStateRepository, SqlMovieRepository, SqlShowRepository,
};
use beam_test_support::postgres::ScopedSchema;
use sea_orm::ConnectionTrait;

struct PgFixture {
    // Held so the schema outlives the repositories that use it, and where
    // a library is seeded.
    schema: ScopedSchema,
    repo: SqlEnrichmentStateRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
}

#[async_trait::async_trait]
impl beam_domain::repositories::contract::fixture::EnrichmentStateFixture for PgFixture {
    fn repo(&self) -> &dyn EnrichmentStateRepository {
        &self.repo
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }

    async fn new_library(&self) -> Uuid {
        beam_test_support::seed::library(self.schema.db().as_ref())
            .await
            .expect("seed a library")
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("enrichment_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlEnrichmentStateRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db),
        schema,
    }
}

beam_domain::enrichment_state_repository_contract!(setup);

/// Two administrators locking one title that has no row yet both succeed,
/// and leave it one row: the upsert is one statement on the title's unique
/// column, not a read and then an insert.
#[tokio::test]
async fn concurrent_locks_of_one_title_leave_one_row() {
    let fixture = setup().await;
    let movie = fixture
        .movies
        .find_or_create_by_identity(CreateMovie::new(
            format!("race {}", Uuid::new_v4()),
            None,
            None,
        ))
        .await
        .unwrap();
    let repo = Arc::new(fixture.repo.clone());
    let target = EnrichmentTargetId::Movie(movie.id);

    let mut tasks = Vec::new();
    for field in MetadataField::ALL.into_iter().take(8) {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.set_locked_fields(target, &[field].into_iter().collect())
                .await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(
            task.await
                .expect("no panic")
                .expect("every lock succeeds")
                .id,
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "one row for the title: {ids:?}");
    assert_eq!(
        fixture
            .repo
            .count(&EnrichmentListFilter::default())
            .await
            .unwrap(),
        1
    );
}

/// The column holds only the names Beam knows: a typo can never lock
/// nothing while reading as a lock.
#[tokio::test]
async fn the_locked_fields_column_refuses_a_name_no_field_has() {
    let fixture = setup().await;
    let movie = fixture
        .movies
        .find_or_create_by_identity(CreateMovie::new(
            format!("check {}", Uuid::new_v4()),
            None,
            None,
        ))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    fixture.repo.ensure_pending(target).await.unwrap();

    let db = fixture.schema.db();
    let refused = db
        .execute_unprepared(&format!(
            "UPDATE metadata_enrichment SET locked_fields = ARRAY['title', 'poster_url'] \
              WHERE movie_id = '{}'",
            movie.id
        ))
        .await;
    assert!(refused.is_err(), "a name no field has is refused");
    db.execute_unprepared(&format!(
        "UPDATE metadata_enrichment SET locked_fields = ARRAY['title', 'poster'] \
          WHERE movie_id = '{}'",
        movie.id
    ))
    .await
    .expect("known names are stored");
}
