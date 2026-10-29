//! The shared `LibraryRepository` contract (issue #349), run against real
//! SQL: a scan's progress written field by field in one `UPDATE`, a no-op
//! for a library that does not exist, and a file count read from the `files`
//! table.
//!
//! Listing and counting are global, so each test owns a migrated schema.

use beam_domain::repositories::{FileRepository, LibraryRepository};
use beam_index::repositories::{SqlFileRepository, SqlLibraryRepository};
use beam_test_support::postgres::ScopedSchema;

struct PgFixture {
    // Held so the schema outlives the repositories that use it.
    _schema: ScopedSchema,
    repo: SqlLibraryRepository,
    files: SqlFileRepository,
}

impl beam_domain::repositories::contract::fixture::LibraryRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn LibraryRepository {
        &self.repo
    }

    fn files(&self) -> &dyn FileRepository {
        &self.files
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("library_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlLibraryRepository::new(db.clone()),
        files: SqlFileRepository::new(db),
        _schema: schema,
    }
}

beam_domain::library_repository_contract!(setup);
