//! The shared `CollectionRepository` contract, run against real SQL: the
//! collection upsert on `external_ref`, the membership's foreign keys, and
//! the listing's grouping and ordering (issue #262).

use beam_domain::repositories::{CollectionRepository, MovieRepository};
use beam_index::repositories::{SqlCollectionRepository, SqlMovieRepository};
use beam_test_support::postgres::ScopedSchema;

struct PgFixture {
    // Held so the schema outlives the repositories that use it.
    _schema: ScopedSchema,
    repo: SqlCollectionRepository,
    movies: SqlMovieRepository,
}

impl beam_domain::repositories::contract::fixture::CollectionRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn CollectionRepository {
        &self.repo
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("collection_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlCollectionRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db),
        _schema: schema,
    }
}

beam_domain::collection_repository_contract!(setup);
