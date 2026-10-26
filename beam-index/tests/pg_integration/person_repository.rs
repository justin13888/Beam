//! The shared `PersonRepository` contract, run against real SQL: the people
//! upsert on `external_ref`, the credits' foreign keys and `CHECK`s, and
//! people search's `ILIKE` and ordering (issue #261).

use beam_domain::repositories::{MovieRepository, PersonRepository, ShowRepository};
use beam_index::repositories::{SqlMovieRepository, SqlPersonRepository, SqlShowRepository};
use beam_test_support::postgres::ScopedSchema;
use beam_test_support::seed;
use uuid::Uuid;

struct PgFixture {
    // Held so the schema outlives the repositories that use it.
    schema: ScopedSchema,
    repo: SqlPersonRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
}

#[async_trait::async_trait]
impl beam_domain::repositories::contract::fixture::PersonRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn PersonRepository {
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
    let schema = ScopedSchema::create_migrated("person_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlPersonRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db),
        schema,
    }
}

beam_domain::person_repository_contract!(setup);
