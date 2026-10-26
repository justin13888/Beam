//! The shared `CollectionRepository` contract, run against real SQL: the
//! collection upsert on `external_ref`, the membership's foreign keys, and
//! the listing's grouping and ordering (issue #262).

use beam_domain::repositories::{CollectionRepository, MovieRepository, ShowRepository};
use beam_index::repositories::{SqlCollectionRepository, SqlMovieRepository, SqlShowRepository};
use beam_test_support::postgres::ScopedSchema;
use beam_test_support::seed;
use uuid::Uuid;

struct PgFixture {
    // Held so the schema outlives the repositories that use it.
    schema: ScopedSchema,
    repo: SqlCollectionRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
}

#[async_trait::async_trait]
impl beam_domain::repositories::contract::fixture::CollectionRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn CollectionRepository {
        &self.repo
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }

    async fn new_library(&self) -> Uuid {
        seed::library(self.schema.db().as_ref())
            .await
            .expect("seed a library")
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("collection_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlCollectionRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db),
        schema,
    }
}

beam_domain::collection_repository_contract!(setup);
