//! The shared `MovieRepository` contract, run against real SQL, plus what only
//! a real Postgres can show: that find-or-create by identity key is atomic,
//! and that deleting an orphaned movie cascades to everything hung off it.
//!
//! As for shows, each contract test owns a migrated schema: `delete_orphaned`
//! is global.

use std::sync::Arc;

use async_trait::async_trait;
use beam_domain::models::enrichment::EnrichmentTargetId;
// `Uuid`, `CreateMovie` and `MovieRepository` come into scope with the
// contract macro below.
use beam_domain::repositories::{EnrichmentStateRepository, FileRepository, GenreRepository};
use beam_index::repositories::{
    SqlEnrichmentStateRepository, SqlFileRepository, SqlGenreRepository, SqlMovieRepository,
};
use beam_test_support::postgres::{self, ScopedSchema};
use beam_test_support::seed;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

struct PgFixture {
    schema: ScopedSchema,
    repo: SqlMovieRepository,
    files: SqlFileRepository,
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::MovieRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn MovieRepository {
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

    async fn new_unkeyed_movie(
        &self,
        title: &str,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        use sea_orm::{ActiveModelTrait, Set};

        let id = Uuid::new_v4();
        let now: chrono::DateTime<chrono::FixedOffset> = created_at.into();
        beam_entity::movie::ActiveModel {
            id: Set(id),
            title: Set(title.to_string()),
            identity_key: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.schema.db().as_ref())
        .await
        .expect("insert a movie with no identity key");
        id
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("movie_contract")
        .await
        .expect("create a migrated schema");
    PgFixture {
        repo: SqlMovieRepository::new(schema.db()),
        files: SqlFileRepository::new(schema.db()),
        schema,
    }
}

beam_domain::movie_repository_contract!(setup);

/// Issue #183 replaced `find_by_title`-then-`create` with one `ON CONFLICT
/// (identity_key)` statement. Under the old pair two files of one new movie,
/// indexed at once by a scan and the watcher, both read "absent" and made two
/// movies -- the second then failing enrichment on the unique `tmdb_id`.
#[tokio::test]
async fn concurrent_find_or_create_by_identity_for_one_key_all_succeed_with_one_row() {
    let db = postgres::connection().await;
    let repo = Arc::new(SqlMovieRepository::new(db));
    let title = format!("Race Movie {}", Uuid::new_v4());

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        let create = CreateMovie::new(title.clone(), Some(1999), None);
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
    assert_eq!(ids.len(), 1, "every call returns the one movie: {ids:?}");
}

async fn count(db: &DatabaseConnection, sql: &str, id: Uuid) -> i64 {
    db.query_one_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        sql,
        [id.into()],
    ))
    .await
    .unwrap()
    .expect("a count row")
    .try_get("", "n")
    .unwrap()
}

/// Deleting an orphaned movie is one `DELETE` on `movies`; the library
/// association, the enrichment row and the genre links go by `ON DELETE
/// CASCADE`, which only the real schema declares.
#[tokio::test]
async fn deleting_an_orphaned_movie_takes_everything_hung_off_it() {
    let schema = ScopedSchema::create_migrated("movie_cascade")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    let movies = SqlMovieRepository::new(db.clone());
    let library = seed::library(db.as_ref()).await.unwrap();
    let movie = movies
        .find_or_create_by_identity(CreateMovie::new("Orphan", None, None))
        .await
        .unwrap();
    movies
        .ensure_library_association(library, movie.id)
        .await
        .unwrap();
    SqlEnrichmentStateRepository::new(db.clone())
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();
    SqlGenreRepository::new(db.clone())
        .set_movie_genres(movie.id, &["Drama".to_string()])
        .await
        .unwrap();

    let removed = movies
        .delete_orphaned(chrono::Utc::now() + chrono::Duration::minutes(1))
        .await
        .unwrap();

    assert_eq!(removed, 1);
    for (what, sql) in [
        (
            "library association",
            "SELECT COUNT(*) AS n FROM library_movies WHERE movie_id = $1",
        ),
        (
            "enrichment state",
            "SELECT COUNT(*) AS n FROM metadata_enrichment WHERE movie_id = $1",
        ),
        (
            "genre links",
            "SELECT COUNT(*) AS n FROM movie_genres WHERE movie_id = $1",
        ),
    ] {
        assert_eq!(count(db.as_ref(), sql, movie.id).await, 0, "{what}");
    }

    schema.drop_schema().await.expect("drop schema");
}
