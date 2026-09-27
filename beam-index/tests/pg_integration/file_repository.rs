//! `SqlFileRepository` against real SQL.
//!
//! The `files.file_status` column is a Postgres *enum* type, while
//! `beam_entity::files::Model` declares the field as `String`. Nothing in the
//! hermetic tier can notice the mismatch -- the in-memory double stores a
//! `String` happily, and `MockDatabase` never type-checks a parameter. Only a
//! real Postgres does.

use std::path::PathBuf;

use beam_domain::models::{CreateMediaFile, FileRelink, FileStatus, MediaFileContent};
use beam_domain::repositories::FileRepository;
use beam_index::repositories::SqlFileRepository;
use beam_test_support::{postgres, seed};

#[tokio::test]
async fn create_persists_a_file_and_reads_it_back_by_path() {
    let db = postgres::connection().await;
    let library_id = seed::library(db.as_ref()).await.unwrap();
    let entry_id = seed::movie_entry(db.as_ref(), library_id).await.unwrap();
    let path = PathBuf::from(format!("/videos/{}/a.mkv", uuid::Uuid::new_v4()));

    let repo = SqlFileRepository::new(db.clone());
    let created = repo
        .create(CreateMediaFile {
            library_id,
            path: path.clone(),
            hash: 0x0123_4567_89ab_cdef,
            size_bytes: 4096,
            mime_type: Some("video/x-matroska".to_string()),
            duration: Some(std::time::Duration::from_secs(120)),
            container_format: Some("matroska".to_string()),
            status: FileStatus::Known,
            classifier_version: 0,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry_id,
            }),
            mtime: None,
        })
        .await
        .expect("inserting a file must work against the real schema");

    let found = repo
        .find_by_path(&path.to_string_lossy())
        .await
        .unwrap()
        .expect("the file just written is readable by path");

    assert_eq!(found.id, created.id);
    assert_eq!(found.status, FileStatus::Known);
    assert_eq!(found.hash, 0x0123_4567_89ab_cdef);
}

#[tokio::test]
async fn find_by_hash_matches_the_full_unsigned_range() {
    let db = postgres::connection().await;
    let library_id = seed::library(db.as_ref()).await.unwrap();
    let entry_id = seed::movie_entry(db.as_ref(), library_id).await.unwrap();
    // A hash above i64::MAX: it is stored in a signed BIGINT column, so the
    // round trip has to reinterpret rather than saturate.
    let hash = u64::MAX - 3;

    let repo = SqlFileRepository::new(db.clone());
    let created = repo
        .create(CreateMediaFile {
            library_id,
            path: PathBuf::from(format!("/videos/{}/b.mkv", uuid::Uuid::new_v4())),
            hash,
            size_bytes: 1,
            mime_type: None,
            duration: None,
            container_format: None,
            status: FileStatus::Unknown,
            classifier_version: 0,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry_id,
            }),
            mtime: None,
        })
        .await
        .unwrap();

    let found = repo.find_by_hash(hash).await.unwrap();
    assert!(
        found.iter().any(|f| f.id == created.id),
        "a hash above i64::MAX must survive the round trip through BIGINT"
    );
}

/// The shared `FileRepository` contract (issue #179's soft-delete lifecycle),
/// run against real SQL -- the same assertions as the in-memory instantiation
/// in `beam-domain/src/repositories/file.rs`. Nested so the names the macro
/// brings into scope do not collide with this file's imports.
mod contract {
    use std::sync::Arc;

    use beam_index::repositories::SqlFileRepository;
    use beam_test_support::{postgres, seed};

    struct PgFixture {
        repo: SqlFileRepository,
        db: Arc<sea_orm::DatabaseConnection>,
    }

    #[async_trait::async_trait]
    impl beam_domain::repositories::contract::fixture::FileRepositoryFixture for PgFixture {
        fn repo(&self) -> &dyn beam_domain::repositories::FileRepository {
            &self.repo
        }

        async fn new_library(&self) -> uuid::Uuid {
            seed::library(&self.db).await.expect("seed a library row")
        }

        async fn new_movie_entry(&self, library_id: uuid::Uuid) -> uuid::Uuid {
            seed::movie_entry(&self.db, library_id)
                .await
                .expect("seed a movie entry")
        }

        async fn new_episode(&self, _library_id: uuid::Uuid) -> uuid::Uuid {
            seed::episode(&self.db).await.expect("seed an episode")
        }
    }

    async fn setup() -> PgFixture {
        let db = postgres::connection().await;
        PgFixture {
            repo: SqlFileRepository::new(db.clone()),
            db,
        }
    }

    beam_domain::file_repository_contract!(setup);
}

/// Marking a file missing is what keeps its playback progress: the row, and so
/// the `playback_progress.file_id` foreign key, is untouched. Only a real
/// Postgres enforces that key.
#[tokio::test]
async fn marking_a_file_missing_keeps_its_playback_progress() {
    let db = postgres::connection().await;
    let user = seed::user(db.as_ref()).await.unwrap();
    let file = seed::file(db.as_ref()).await.unwrap();
    insert_progress(db.as_ref(), user, file).await;

    let repo = SqlFileRepository::new(db.clone());
    assert_eq!(
        repo.mark_missing(vec![file], chrono::Utc::now())
            .await
            .unwrap(),
        1
    );

    assert_eq!(progress_rows(db.as_ref(), file).await, 1);
}

/// Purging is the one delete, and `ON DELETE CASCADE` takes the progress with
/// it rather than failing on the foreign key.
#[tokio::test]
async fn purging_a_missing_file_cascades_to_its_playback_progress() {
    let db = postgres::connection().await;
    let user = seed::user(db.as_ref()).await.unwrap();
    let file = seed::file(db.as_ref()).await.unwrap();
    insert_progress(db.as_ref(), user, file).await;

    let repo = SqlFileRepository::new(db.clone());
    repo.mark_missing(vec![file], chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(repo.purge_missing(vec![file]).await.unwrap(), 1);

    assert_eq!(progress_rows(db.as_ref(), file).await, 0);
}

/// A relinked file keeps its playback progress (issue #180): the relink is
/// an update of the row the `playback_progress.file_id` foreign key points
/// at, never a delete and re-insert that would cascade the progress away.
#[tokio::test]
async fn relinking_a_missing_file_keeps_its_playback_progress() {
    let db = postgres::connection().await;
    let user = seed::user(db.as_ref()).await.unwrap();
    let file = seed::file(db.as_ref()).await.unwrap();
    insert_progress(db.as_ref(), user, file).await;

    let repo = SqlFileRepository::new(db.clone());
    repo.mark_missing(vec![file], chrono::Utc::now())
        .await
        .unwrap();
    let moved_to = PathBuf::from(format!("/videos/{}/moved.mkv", uuid::Uuid::new_v4()));
    repo.relink(
        vec![FileRelink {
            id: file,
            path: moved_to.clone(),
            size_bytes: 2048,
            mtime: None,
        }],
        Vec::new(),
        chrono::Utc::now(),
    )
    .await
    .expect("relink against the real schema");

    let relinked = repo.find_by_id(file).await.unwrap().expect("visible again");
    assert_eq!(relinked.path, moved_to);
    assert_eq!(relinked.missing_since, None);
    assert_eq!(progress_rows(db.as_ref(), file).await, 1);
}

/// Two files that swapped names swap paths (issue #180) under the real
/// `idx_files_path_unique`, which Postgres checks per statement: moving one
/// row straight onto the other's path is refused, and the swap -- one call
/// -- goes through, each row keeping its playback progress.
#[tokio::test]
async fn a_swap_exchanges_paths_under_the_unique_path_index() {
    let db = postgres::connection().await;
    let user = seed::user(db.as_ref()).await.unwrap();
    let heat = seed::file(db.as_ref()).await.unwrap();
    let ronin = seed::file(db.as_ref()).await.unwrap();
    insert_progress(db.as_ref(), user, heat).await;
    insert_progress(db.as_ref(), user, ronin).await;
    let repo = SqlFileRepository::new(db.clone());
    let path_of = |id| {
        let repo = &repo;
        async move { repo.find_by_id(id).await.unwrap().unwrap().path }
    };
    let (heat_path, ronin_path) = (path_of(heat).await, path_of(ronin).await);
    let onto = |id, path: &PathBuf| FileRelink {
        id,
        path: path.clone(),
        size_bytes: 1024,
        mtime: None,
    };

    let alone = repo
        .relink(
            vec![onto(heat, &ronin_path)],
            Vec::new(),
            chrono::Utc::now(),
        )
        .await;
    let err = alone.expect_err("the path is held by a row outside the call");
    assert!(
        err.to_string().contains("idx_files_path_unique"),
        "refused by the unique index, got: {err}"
    );
    assert_eq!(path_of(heat).await, heat_path, "and nothing moved");

    repo.relink(
        vec![onto(heat, &ronin_path), onto(ronin, &heat_path)],
        Vec::new(),
        chrono::Utc::now(),
    )
    .await
    .expect("the swap goes through");

    assert_eq!(path_of(heat).await, ronin_path);
    assert_eq!(path_of(ronin).await, heat_path);
    assert_eq!(progress_rows(db.as_ref(), heat).await, 1);
    assert_eq!(progress_rows(db.as_ref(), ronin).await, 1);
}

async fn insert_progress(db: &sea_orm::DatabaseConnection, user: uuid::Uuid, file: uuid::Uuid) {
    use sea_orm::{ConnectionTrait, Statement};

    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO playback_progress \
         (id, user_id, file_id, position_secs, duration_secs, completed, updated_at) \
         VALUES ($1, $2, $3, 1.0, 100.0, false, now())",
        [uuid::Uuid::new_v4().into(), user.into(), file.into()],
    ))
    .await
    .expect("insert progress");
}

async fn progress_rows(db: &sea_orm::DatabaseConnection, file: uuid::Uuid) -> usize {
    use sea_orm::{ConnectionTrait, Statement};

    db.query_all_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM playback_progress WHERE file_id = $1",
        [file.into()],
    ))
    .await
    .expect("query progress")
    .len()
}
