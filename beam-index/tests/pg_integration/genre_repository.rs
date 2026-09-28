//! The shared `GenreRepository` contract, run against real SQL: the genre
//! upsert by slug, the junction tables' foreign keys, and the batched name
//! reads a catalogue page makes (issue #187).

use beam_domain::repositories::{GenreRepository, MovieRepository, ShowRepository};
use beam_index::repositories::{SqlGenreRepository, SqlMovieRepository, SqlShowRepository};
use beam_test_support::postgres::ScopedSchema;

struct PgFixture {
    // Held so the schema outlives the repositories that use it.
    _schema: ScopedSchema,
    repo: SqlGenreRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
}

impl beam_domain::repositories::contract::fixture::GenreRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn GenreRepository {
        &self.repo
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }
}

async fn setup() -> PgFixture {
    let schema = ScopedSchema::create_migrated("genre_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlGenreRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db),
        _schema: schema,
    }
}

beam_domain::genre_repository_contract!(setup);
