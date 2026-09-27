//! The shared `LibraryShapeRepository` contract (issue #93), run against real
//! SQL -- the same assertions as the in-memory instantiation in
//! `beam-domain/src/repositories/library_shape.rs`.
//!
//! The shape is global, so each contract test owns a migrated schema: over a
//! shared database it would count every other test's rows.

use beam_domain::repositories::{
    FileRepository, LibraryRepository, LibraryShapeRepository, MediaStreamRepository,
    MovieRepository, ShowRepository,
};
use beam_index::repositories::{
    SqlFileRepository, SqlLibraryRepository, SqlLibraryShapeRepository, SqlMediaStreamRepository,
    SqlMovieRepository, SqlShowRepository,
};
use beam_test_support::postgres::ScopedSchema;

struct PgFixture {
    // Held so the schema outlives the repositories built on it.
    _schema: ScopedSchema,
    repo: SqlLibraryShapeRepository,
    libraries: SqlLibraryRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
    files: SqlFileRepository,
    streams: SqlMediaStreamRepository,
}

impl beam_domain::repositories::contract::fixture::LibraryShapeFixture for PgFixture {
    fn repo(&self) -> &dyn LibraryShapeRepository {
        &self.repo
    }

    fn libraries(&self) -> &dyn LibraryRepository {
        &self.libraries
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }

    fn files(&self) -> &dyn FileRepository {
        &self.files
    }

    fn streams(&self) -> &dyn MediaStreamRepository {
        &self.streams
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("library_shape_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlLibraryShapeRepository::new(db.clone()),
        libraries: SqlLibraryRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db.clone()),
        files: SqlFileRepository::new(db.clone()),
        streams: SqlMediaStreamRepository::new(db),
        _schema: schema,
    }
}

beam_domain::library_shape_repository_contract!(setup);
